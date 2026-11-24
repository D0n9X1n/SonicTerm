//! Pins the control tools at their boundaries with the rest of the app, through the real tool and
//! loop paths: every control tool refuses out of scope or in READONLY without writing or changing
//! topology, queued input refuses a run, only typing stamps the pane, an agent closing a child
//! window's last tab reaps that window, a helper created with argv is held after a real exit
//! that `wait_for` reports, `list_panes` titles respect the read scope and one reply budget, and
//! a whole `list_panes` result stays inside its bound.

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
    app.services.mcp_server = Some(McpServer {
        shared: Arc::clone(&shared),
        endpoint: "test-endpoint".into(),
        _endpoint_guard: None,
        config,
        charges: HashMap::new(),
        owner,
        read_notice_due: false,
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
    tick: impl FnMut(&mut App),
) -> Value {
    call_through_loop_within(app, shared, name, arguments, None, tick)
}

/// [`call_through_loop`], with the worker's `list_panes` whole-result bound set to `limit_bytes`
/// when given, so a few panes can reach it.
fn call_through_loop_within(
    app: &mut App,
    shared: &Arc<McpShared>,
    name: &str,
    arguments: Value,
    limit_bytes: Option<usize>,
    mut tick: impl FnMut(&mut App),
) -> Value {
    let worker_shared = Arc::clone(shared);
    let params = json!({ "name": name, "arguments": arguments });
    let worker = std::thread::spawn(move || {
        crate::app::mcp::reply_budget::REPLY_LIMIT_OVERRIDE.set(limit_bytes);
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
    let pane = app
        .registry
        .windows
        .values_mut()
        .find_map(|window| window.panes.get_mut(&pane_id))
        .unwrap();
    pane.pty = Some(pty);
    pane.creator = crate::app::mcp::agents::PaneOwner::agent_if(agent_owned);
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
    let mut shape: Vec<_> = app
        .registry
        .windows
        .values()
        .map(|window| (window.tab_states.len(), window.panes.len()))
        .collect();
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
    app.testing.pty_write_log_enabled = true;
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
            assert!(app.testing.test_pty_writes.lock().is_empty(), "{label} wrote");
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
    let reply = app.mcp_run_start(McpControl::Agent.into(), pane_id, "ls");
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
    let child_pane = *app.registry.windows[&child].panes.keys().next().unwrap();
    app.registry.windows.get_mut(&child).unwrap().panes.get_mut(&child_pane).unwrap().creator =
        crate::app::mcp::agents::PaneOwner::agent_if(true);
    assert!(matches!(app.mcp_close_pane(McpControl::Agent.into(), child_pane), LoopReply::Closed));
    assert!(!app.registry.windows.contains_key(&child), "the emptied child window is reaped");
    assert!(app.mcp_pane_entries().iter().all(|entry| entry.pane_id != child_pane));
    assert!(matches!(
        app.mcp_close_pane(McpControl::Any.into(), main_pane),
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
    app.main_mut().unwrap().panes.get_mut(&anchor).unwrap().creator =
        crate::app::mcp::agents::PaneOwner::Agent(crate::app::mcp::agents::AgentSessionId::next());
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

/// Refresh tab `tab_index`'s label from its active pane, as the redraw does for the visible tab.
fn refresh_label(app: &mut App, tab_index: usize) {
    let window = app.main_mut().unwrap();
    window.tabs.activate(tab_index);
    let source = window.tab_states[tab_index].active_pane;
    let crate::app::WindowState { tabs, panes, .. } = window;
    let parser = panes[&source].parser.lock();
    crate::app::refresh_active_tab_title(tabs, source, &panes[&source], &parser, tab_index);
}

/// The listed pane with `pane_id` in a `list_panes` result.
fn listed(result: &Value, pane_id: u64) -> Value {
    let wanted = pane_id.to_string();
    let panes = result["structuredContent"]["panes"].as_array().expect("panes");
    panes.iter().find(|pane| pane["pane_id"] == wanted.as_str()).expect("listed").clone()
}

/// Under `read = "agent"`, a tab label taken from a pane the caller may not read is withheld, with
/// an explicit marker, from every pane of the tab, even a readable agent split beside it; once the
/// agent's own pane is the label's source, the label is listed.
#[test]
fn list_panes_withholds_a_tab_label_from_an_unreadable_pane() {
    let mut app = app();
    let user = app.__test_seed_tab("user");
    app.__test_set_main_pane_viewport(
        sonicterm_ui::pane::Rect::new(0.0, 0.0, 800.0, 240.0),
        10.0,
        10.0,
    );
    app.__test_split_active_right();
    let window = app.main_mut().unwrap();
    let agent = window.tab_states[0].active_pane;
    window.panes.get_mut(&agent).unwrap().creator =
        crate::app::mcp::agents::PaneOwner::agent_if(true);
    window.panes[&user].parser.lock().advance(b"\x1b]1;user-secret\x07");
    window.panes[&agent].parser.lock().advance(b"\x1b]1;agent-name\x07");
    // The user's pane is active, so the tab shows the user's OSC 1 name.
    window.tab_states[0].active_pane = user;
    refresh_label(&mut app, 0);
    let shared = install(&mut app, McpControl::Agent);
    shared.set_read(sonicterm_cfg::config::McpRead::Agent);
    let result = call_through_loop(&mut app, &shared, "list_panes", json!({}), |_| {});
    let panes = result["structuredContent"]["panes"].as_array().unwrap();
    assert_eq!(panes.len(), 1, "only the agent's pane is readable: {result}");
    let pane = listed(&result, agent);
    assert_eq!(pane["tab_title"], Value::Null);
    assert_eq!(pane["omitted"]["tab_title"], "unreadable");
    assert_eq!(pane["icon_title"], "agent-name", "the pane's own name is readable");
    assert!(!result.to_string().contains("user-secret"), "the user's label leaked: {result}");
    // The agent's own pane becomes the source: the label it gives is listed.
    app.main_mut().unwrap().tab_states[0].active_pane = agent;
    refresh_label(&mut app, 0);
    let result = call_through_loop(&mut app, &shared, "list_panes", json!({}), |_| {});
    let pane = listed(&result, agent);
    assert!(pane["tab_title"].as_str().is_some_and(|label| label.ends_with("agent-name")));
    assert_eq!(pane["omitted"], Value::Null, "nothing was omitted: {pane}");
}

/// Many panes with maximal titles cannot push a `list_panes` reply toward the bridge's 1 MiB line
/// limit: the title fields share one byte budget per reply, a field past it is `null` with an
/// explicit `"budget"` marker, the reply still succeeds and is `partial`, and the first panes keep
/// their titles.
#[test]
fn list_panes_bounds_every_title_by_one_reply_budget() {
    // The bridge's relay line limit (`sonicterm_mcp::relay::MAX_LINE_BYTES`).
    const RELAY_LINE_BYTES: usize = 1 << 20;
    let mut app = app();
    let name = "n".repeat(16 * 1024);
    let mut panes = Vec::new();
    for tab_index in 0..24 {
        let pane_id = app.__test_seed_tab("tab");
        let window = app.main_mut().unwrap();
        let sequence = format!("\x1b]1;{name}\x07\x1b]2;{name}\x07");
        window.panes[&pane_id].parser.lock().advance(sequence.as_bytes());
        refresh_label(&mut app, tab_index);
        panes.push(pane_id);
    }
    let shared = install(&mut app, McpControl::Agent);
    shared.set_read(sonicterm_cfg::config::McpRead::Any);
    let result = call_through_loop(&mut app, &shared, "list_panes", json!({}), |_| {});
    assert_eq!(result["isError"], false);
    let wire_bytes = result.to_string().len();
    assert!(wire_bytes < RELAY_LINE_BYTES / 2, "the reply took {wire_bytes} bytes");
    let first = listed(&result, panes[0]);
    assert_eq!(first["icon_title"], name.as_str(), "the first pane keeps its name");
    assert_eq!(first["title"], name.as_str());
    let last = listed(&result, panes[23]);
    assert_eq!(last["title"], Value::Null);
    assert_eq!(last["omitted"]["title"], "budget", "{}", last["omitted"]);
    assert_eq!(last["omitted"]["icon_title"], "budget");
    assert_eq!(last["omitted"]["tab_title"], "budget");
    // Every pane is listed, so only the budget omission makes the result partial.
    assert_eq!(result["structuredContent"]["truncated_panes"], 0);
    assert_eq!(result["structuredContent"]["partial"], true, "a title omission is partial");
}

/// A pane's OSC 1337 user variables as `list_panes` lists them: `(name, value)` pairs in order.
fn listed_user_vars(pane: &Value) -> Vec<(String, String)> {
    let entries = pane["user_vars"].as_array().expect("user_vars is a list");
    let field = |entry: &Value, key: &str| entry[key].as_str().expect("a string").to_owned();
    entries.iter().map(|entry| (field(entry, "name"), field(entry, "value"))).collect()
}

/// Under `read = "agent"`, `list_panes` carries each readable pane's user variables in the order
/// they were set, through the real tool and loop paths; a pane the caller may not read is not
/// listed, and none of its names or values reach the reply.
#[test]
fn list_panes_lists_readable_user_vars_in_insertion_order() {
    let mut app = app();
    let user = app.__test_seed_tab("user");
    app.__test_set_main_pane_viewport(
        sonicterm_ui::pane::Rect::new(0.0, 0.0, 800.0, 240.0),
        10.0,
        10.0,
    );
    app.__test_split_active_right();
    let window = app.main_mut().unwrap();
    let agent = window.tab_states[0].active_pane;
    window.panes.get_mut(&agent).unwrap().creator =
        crate::app::mcp::agents::PaneOwner::agent_if(true);
    // `user-secret` in base64, so only a leak of the decoded value could put it in the reply.
    window.panes[&user].parser.lock().advance(b"\x1b]1337;SetUserVar=private=dXNlci1zZWNyZXQ=\x07");
    // `zeta` is set before `alpha`, so a sorted listing would differ from insertion order.
    window.panes[&agent]
        .parser
        .lock()
        .advance(b"\x1b]1337;SetUserVar=zeta=MQ==;SetUserVar=alpha=Mg==\x07");
    let shared = install(&mut app, McpControl::Agent);
    shared.set_read(sonicterm_cfg::config::McpRead::Agent);
    let result = call_through_loop(&mut app, &shared, "list_panes", json!({}), |_| {});
    let panes = result["structuredContent"]["panes"].as_array().unwrap();
    assert_eq!(panes.len(), 1, "only the agent's pane is readable: {result}");
    let pane = listed(&result, agent);
    let expected = [("zeta", "1"), ("alpha", "2")].map(|(name, value)| (name.into(), value.into()));
    assert_eq!(listed_user_vars(&pane), expected);
    let wire = result.to_string();
    assert!(!wire.contains("private") && !wire.contains("user-secret"), "leaked: {wire}");
}

/// Many panes with full user-variable stores share the one reply budget with the titles: later
/// panes list `user_vars` as `null` with a `"budget"` marker, never a cut list, the first pane
/// keeps its whole list, the omission makes the reply `partial`, and the reply stays well inside
/// the bridge's line limit.
#[test]
fn list_panes_bounds_user_vars_by_the_reply_budget() {
    // The bridge's relay line limit (`sonicterm_mcp::relay::MAX_LINE_BYTES`).
    const RELAY_LINE_BYTES: usize = 1 << 20;
    use base64::Engine;
    let mut app = app();
    let value = base64::engine::general_purpose::STANDARD.encode([b'v'; 4000]);
    // Four 4002-byte variables fill most of a pane's 16 KiB store.
    let records: Vec<String> = (0..4).map(|index| format!("SetUserVar=n{index}={value}")).collect();
    let sequence = format!("\x1b]1337;{}\x07", records.join(";"));
    let mut panes = Vec::new();
    for _ in 0..20 {
        let pane_id = app.__test_seed_tab("tab");
        let window = app.main_mut().unwrap();
        window.panes[&pane_id].parser.lock().advance(sequence.as_bytes());
        panes.push(pane_id);
    }
    let shared = install(&mut app, McpControl::Agent);
    shared.set_read(sonicterm_cfg::config::McpRead::Any);
    let result = call_through_loop(&mut app, &shared, "list_panes", json!({}), |_| {});
    assert_eq!(result["isError"], false);
    let wire_bytes = result.to_string().len();
    assert!(wire_bytes < RELAY_LINE_BYTES / 2, "the reply took {wire_bytes} bytes");
    let first = listed(&result, panes[0]);
    assert_eq!(listed_user_vars(&first).len(), 4, "the first pane keeps its whole list");
    let last = listed(&result, panes[19]);
    assert_eq!(last["user_vars"], Value::Null);
    assert_eq!(last["omitted"]["user_vars"], "budget", "{}", last["omitted"]);
    assert_eq!(result["structuredContent"]["truncated_panes"], 0);
    assert_eq!(result["structuredContent"]["partial"], true, "a user_vars omission is partial");
}

/// Through the real tool and loop paths under `read = "agent"`, `list_panes` lists a readable
/// pane's OSC 7501 status as typed metadata only (`program_state` and `program_status` records
/// with state, kind and progress); its id, app, title and message appear only after the user's
/// `program_status_text` opt-in, and an unreadable pane's status never reaches the reply.
#[test]
fn list_panes_lists_typed_program_status_and_text_only_on_opt_in() {
    use base64::Engine;
    let encode = |text: &str| base64::engine::general_purpose::STANDARD.encode(text);
    let mut app = app();
    let user = app.__test_seed_tab("user");
    app.__test_set_main_pane_viewport(
        sonicterm_ui::pane::Rect::new(0.0, 0.0, 800.0, 240.0),
        10.0,
        10.0,
    );
    app.__test_split_active_right();
    let window = app.main_mut().unwrap();
    let agent = window.tab_states[0].active_pane;
    window.panes.get_mut(&agent).unwrap().creator =
        crate::app::mcp::agents::PaneOwner::agent_if(true);
    let private = format!("\x1b]7501;app=userapp:state=error:msg={}\x07", encode("user-secret"));
    window.panes[&user].parser.lock().advance(private.as_bytes());
    let report = format!(
        "\x1b]7501;id=job:app=claude-code:state=blocked:kind=permission:progress=30:title={}:msg={}\x07",
        encode("Approve?"),
        encode("agent-message")
    );
    window.panes[&agent].parser.lock().advance(report.as_bytes());
    let shared = install(&mut app, McpControl::Agent);
    shared.set_read(sonicterm_cfg::config::McpRead::Agent);
    let result = call_through_loop(&mut app, &shared, "list_panes", json!({}), |_| {});
    let pane = listed(&result, agent);
    assert_eq!(pane["program_state"], "blocked");
    assert_eq!(
        pane["program_status"],
        json!([{ "state": "blocked", "kind": "permission", "progress": 30 }])
    );
    let wire = result.to_string();
    for hidden in ["job", "claude-code", "Approve?", "agent-message", "userapp", "user-secret"] {
        assert!(!wire.contains(hidden), "{hidden} leaked without the opt-in: {wire}");
    }
    shared.set_program_status_text(true);
    let result = call_through_loop(&mut app, &shared, "list_panes", json!({}), |_| {});
    let pane = listed(&result, agent);
    assert_eq!(
        pane["program_status"],
        json!([{
            "state": "blocked", "kind": "permission", "progress": 30,
            "id": "job", "app": "claude-code", "title": "Approve?", "msg": "agent-message"
        }])
    );
    let wire = result.to_string();
    assert!(!wire.contains("userapp") && !wire.contains("user-secret"), "leaked: {wire}");
    // An unreadable pane is filtered by scope, not omitted by a budget, so nothing is partial.
    assert_eq!(result["structuredContent"]["partial"], false);
}

/// Opted-in status lists have their own 64 KiB sublimit inside the shared reply budget: once the
/// lists listed so far fill it, a later pane lists `program_status` as `null` with a `"budget"`
/// marker, never a cut list, while the shared budget still lists its title and its lightweight
/// `program_state`, and the omission makes the whole result `partial`.
#[test]
fn list_panes_bounds_program_status_by_its_own_sublimit() {
    use base64::Engine;
    // Each quote serializes as two bytes, so one record's message costs about 4 KiB on the wire
    // and eight of them about 33 KiB: one pane's list fits the 64 KiB sublimit, two do not, and
    // twelve lists together stay far below the shared 256 KiB budget.
    let msg = base64::engine::general_purpose::STANDARD.encode([b'"'; 2048]);
    let sequence: String =
        (0..8).map(|index| format!("\x1b]7501;id=r{index}:state=working:msg={msg}\x07")).collect();
    let mut app = app();
    let mut panes = Vec::new();
    for _ in 0..12 {
        let pane_id = app.__test_seed_tab("tab");
        let window = app.main_mut().unwrap();
        let mut parser = window.panes[&pane_id].parser.lock();
        parser.advance(sequence.as_bytes());
        parser.advance(b"\x1b]2;job\x07");
        drop(parser);
        panes.push(pane_id);
    }
    let shared = install(&mut app, McpControl::Agent);
    shared.set_read(sonicterm_cfg::config::McpRead::Any);
    shared.set_program_status_text(true);
    let result = call_through_loop(&mut app, &shared, "list_panes", json!({}), |_| {});
    assert_eq!(result["isError"], false);
    let first = listed(&result, panes[0]);
    assert_eq!(first["program_status"].as_array().map(Vec::len), Some(8), "kept whole");
    let second = listed(&result, panes[1]);
    assert_eq!(second["program_status"], Value::Null, "past the sublimit");
    assert_eq!(second["omitted"]["program_status"], "budget");
    assert_eq!(second["program_state"], "working", "the lightweight state stays");
    let last = listed(&result, panes[11]);
    assert_eq!(last["title"], "job", "the shared budget is not spent by the status lists");
    assert_eq!(last["omitted"]["program_status"], "budget");
    assert_eq!(result["structuredContent"]["truncated_panes"], 0);
    assert_eq!(result["structuredContent"]["partial"], true, "a status omission is partial");
}

/// Many panes with long OSC 7 directories share the one reply budget with the titles, so the
/// complete JSON-RPC response line stays below the bridge's 1 MiB limit instead of closing the
/// connection: later panes list `cwd` as `null` with a `"budget"` marker, the first keeps its own,
/// and the omission makes the reply `partial`.
#[test]
fn list_panes_charges_every_cwd_to_the_reply_budget() {
    use crate::app::mcp::jsonrpc::{response, MAX_LINE_BYTES};
    let mut app = app();
    // Each directory serializes to about 14 KiB, so 80 of them would pass 1 MiB on their own.
    let directory = format!("/{}", "d".repeat(14 * 1024));
    let mut panes = Vec::new();
    for _ in 0..80 {
        let pane_id = app.__test_seed_tab("tab");
        let window = app.main_mut().unwrap();
        let sequence = format!("\x1b]7;file://{directory}\x07");
        window.panes[&pane_id].parser.lock().advance(sequence.as_bytes());
        panes.push(pane_id);
    }
    let shared = install(&mut app, McpControl::Agent);
    shared.set_read(sonicterm_cfg::config::McpRead::Any);
    let result = call_through_loop(&mut app, &shared, "list_panes", json!({}), |_| {});
    assert_eq!(result["isError"], false);
    let line_bytes = response(&json!(1), result.clone()).len();
    assert!(line_bytes < MAX_LINE_BYTES, "the response line took {line_bytes} bytes");
    let first = listed(&result, panes[0]);
    assert_eq!(first["cwd"], directory.as_str(), "the first pane keeps its directory");
    let last = listed(&result, panes[79]);
    assert_eq!(last["cwd"], Value::Null);
    assert_eq!(last["omitted"]["cwd"], "budget", "{}", last["omitted"]);
    assert_eq!(result["structuredContent"]["truncated_panes"], 0);
    assert_eq!(result["structuredContent"]["partial"], true, "a cwd omission is partial");
}

/// The whole `list_panes` result, fixed fields and every pane record, stays within its bound:
/// once the next pane would cross it, no later pane is listed, the result is `partial` and
/// `truncated_panes` counts the panes left out, so no reply reaches the bridge's line limit.
#[test]
fn list_panes_stops_listing_at_the_whole_result_bound() {
    const LIMIT_BYTES: usize = 8 * 1024;
    let mut app = app();
    let mut panes = Vec::new();
    for _ in 0..40 {
        let pane_id = app.__test_seed_tab("tab");
        let window = app.main_mut().unwrap();
        window.panes[&pane_id].parser.lock().advance(b"\x1b]7;file:///tmp/work\x07\x1b]2;job\x07");
        panes.push(pane_id);
    }
    let shared = install(&mut app, McpControl::Agent);
    shared.set_read(sonicterm_cfg::config::McpRead::Any);
    let result = call_through_loop_within(
        &mut app,
        &shared,
        "list_panes",
        json!({}),
        Some(LIMIT_BYTES),
        |_| {},
    );
    assert_eq!(result["isError"], false);
    let wire_bytes = result.to_string().len();
    assert!(wire_bytes <= LIMIT_BYTES, "the result took {wire_bytes} bytes");
    let structured = &result["structuredContent"];
    let listed_panes = structured["panes"].as_array().expect("panes");
    let truncated = structured["truncated_panes"].as_u64().expect("truncated_panes");
    assert!(truncated > 0 && !listed_panes.is_empty(), "{structured}");
    assert_eq!(listed_panes.len() as u64 + truncated, 40, "every pane is listed or counted");
    assert_eq!(structured["partial"], true);
    // The panes listed are the first ones, in order: the cut drops only the tail.
    let listed_ids: Vec<&str> =
        listed_panes.iter().filter_map(|pane| pane["pane_id"].as_str()).collect();
    let expected: Vec<String> = panes[..listed_panes.len()].iter().map(u64::to_string).collect();
    assert_eq!(listed_ids, expected);
}
