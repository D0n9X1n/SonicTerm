//! Pins the control tools' loop side: the scope matrix, READONLY, paste wrapping and keys reaching
//! the pane as bytes, the run reservation refusing other agent writes, and `close_pane` closing
//! exactly one pane, its tab, or nothing for the main window's last tab.

use std::sync::atomic::Ordering;

use sonicterm_cfg::config::{Config, McpControl};
use sonicterm_cfg::keymap::Keymap;
use sonicterm_cfg::theme::Theme;
use sonicterm_ui::copy_mode::CopyModeState;
use sonicterm_vt::vt::CaptureStagingPool;

use super::*;
use crate::app::mcp::keys;
use crate::app::media::InlineMediaPool;
use crate::app::pane_target::Placement;

/// An app with two main tabs: a user pane and an agent pane, each with a live `cat` (or `cmd`)
/// PTY so writes are admitted; every write is also recorded.
fn app_with_user_and_agent_panes() -> (App, u64, u64) {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default())
        .with_capture_staging_pool(CaptureStagingPool::new())
        .with_inline_media_pool(InlineMediaPool::new());
    let user = app.__test_seed_tab("user");
    let agent = app.__test_seed_tab("agent");
    for pane_id in [user, agent] {
        #[cfg(unix)]
        let (program, args) = ("/bin/cat", Vec::<String>::new());
        #[cfg(windows)]
        let (program, args) = ("cmd.exe", vec!["/D".to_owned(), "/Q".to_owned()]);
        let pty =
            sonicterm_io::pty::PtyHandle::spawn_with_args(program, &args, 80, 24).expect("pty");
        let pane = app.main_mut().unwrap().panes.get_mut(&pane_id).unwrap();
        pane.pty = Some(pty);
        pane.agent_owned = pane_id == agent;
    }
    app.testing.pty_write_log_enabled = true;
    (app, user, agent)
}

/// The bytes written to `pane_id` so far.
fn written(app: &App, pane_id: u64) -> Vec<u8> {
    app.testing
        .test_pty_writes
        .lock()
        .iter()
        .filter(|(id, _)| *id == pane_id)
        .flat_map(|(_, bytes)| bytes.clone())
        .collect()
}

/// A refusal's code, or `None` when the call did its work.
fn refusal(reply: &LoopReply) -> Option<&'static str> {
    match reply {
        LoopReply::Refused(code, _) => Some(code),
        _ => None,
    }
}

/// Off refuses everything; agent scope allows the agent's pane only, and creating a helper beside
/// any pane; any scope allows every pane.
#[test]
fn the_scope_matrix_holds_for_every_control_tool() {
    let (app, user, agent) = app_with_user_and_agent_panes();
    let cases = [
        (McpControl::Off, agent, false, Some(Refusal::ControlOff)),
        (McpControl::Off, user, true, Some(Refusal::ControlOff)),
        (McpControl::Agent, user, false, Some(Refusal::OutOfScope)),
        (McpControl::Agent, agent, false, None),
        (McpControl::Agent, user, true, None),
        (McpControl::Any, user, false, None),
        (McpControl::Any, agent, false, None),
    ];
    for (control, pane_id, anchor_only, expected) in cases {
        let got = app.authorize_control(control, pane_id, anchor_only).err();
        assert_eq!(got, expected, "{control:?} pane {pane_id} anchor {anchor_only}");
    }
    assert_eq!(app.authorize_control(McpControl::Any, 999_999, false), Err(Refusal::PaneGone));
}

/// READONLY in the pane's window refuses every control tool, creation included, under any scope.
#[test]
fn readonly_refuses_every_control_tool() {
    let (mut app, user, agent) = app_with_user_and_agent_panes();
    app.main_mut().unwrap().copy_mode = Some(CopyModeState::read_only_at((0, 0)));
    for pane_id in [user, agent] {
        assert_eq!(app.authorize_control(McpControl::Any, pane_id, true), Err(Refusal::ReadOnly));
    }
    let reply = app.mcp_send_text(McpControl::Any, agent, "x", false);
    assert_eq!(refusal(&reply), Some("readonly"));
    assert!(written(&app, agent).is_empty());
}

/// Text is written plain, or wrapped in bracketed paste when the pane asked for it; `submit`
/// adds one carriage return after the paste.
#[test]
fn send_text_wraps_a_paste_when_the_pane_asked() {
    let (mut app, _user, agent) = app_with_user_and_agent_panes();
    assert!(matches!(
        app.mcp_send_text(McpControl::Agent, agent, "ls", true),
        LoopReply::Written(3, _)
    ));
    assert_eq!(written(&app, agent), b"ls\r");
    app.testing.test_pty_writes.lock().clear();
    let parser = std::sync::Arc::clone(&app.main().unwrap().panes[&agent].parser);
    parser.lock().advance(b"\x1b[?2004h");
    app.mcp_send_text(McpControl::Agent, agent, "ls", true);
    assert_eq!(written(&app, agent), b"\x1b[200~ls\x1b[201~\r");
}

/// Keys follow the pane's cursor mode, read just before encoding.
#[test]
fn send_keys_follow_the_pane_modes() {
    let (mut app, _user, agent) = app_with_user_and_agent_panes();
    let keys = keys::parse_keys(&["Up".to_owned(), "Ctrl+C".to_owned()]).unwrap();
    app.mcp_send_keys(McpControl::Agent, agent, &keys);
    assert_eq!(written(&app, agent), b"\x1b[A\x03");
    app.testing.test_pty_writes.lock().clear();
    let pane = &app.main().unwrap().panes[&agent];
    pane.parser.lock().advance(b"\x1b[?1h");
    // The VT worker publishes keyboard modes in production; the test publishes them by hand.
    let snapshot = pane.parser.lock().keyboard_input_snapshot();
    pane.keyboard_input.store(snapshot, Ordering::Relaxed);
    app.mcp_send_keys(McpControl::Agent, agent, &keys[..1]);
    assert_eq!(written(&app, agent), b"\x1bOA");
}

/// While an agent run owns the pane, every other agent write is refused and nothing is written.
#[test]
fn a_run_reservation_refuses_other_agent_writes() {
    let (mut app, _user, agent) = app_with_user_and_agent_panes();
    app.main().unwrap().panes[&agent].agent_run.store(true, Ordering::Release);
    assert_eq!(refusal(&app.mcp_send_text(McpControl::Agent, agent, "x", false)), Some("busy"));
    let keys = keys::parse_keys(&["Enter".to_owned()]).unwrap();
    assert_eq!(refusal(&app.mcp_send_keys(McpControl::Agent, agent, &keys)), Some("busy"));
    assert!(written(&app, agent).is_empty());
}

/// `run_command` writes nothing without shell integration, with a draft typed, or with a control
/// character; on a clean marked prompt it writes the command and a carriage return and holds the
/// pane until the run ends.
#[test]
fn run_start_writes_only_at_a_clean_prompt() {
    let (mut app, _user, agent) = app_with_user_and_agent_panes();
    let parser = std::sync::Arc::clone(&app.main().unwrap().panes[&agent].parser);
    assert_eq!(
        refusal(&app.mcp_run_start(McpControl::Agent, agent, "ls")),
        Some("no_shell_integration")
    );
    parser.lock().advance(b"\x1b]133;A\x07$ \x1b]133;B\x07draft");
    assert_eq!(refusal(&app.mcp_run_start(McpControl::Agent, agent, "ls")), Some("draft_present"));
    parser.lock().advance(b"\r\n\x1b]133;A\x07$ \x1b]133;B\x07");
    assert_eq!(
        refusal(&app.mcp_run_start(McpControl::Agent, agent, "ls\nrm")),
        Some("invalid_command")
    );
    assert!(written(&app, agent).is_empty());
    let LoopReply::RunStarted(started) = app.mcp_run_start(McpControl::Agent, agent, "ls") else {
        panic!("the run should start");
    };
    assert_eq!(written(&app, agent), b"ls\r");
    assert!(started.entry.agent_run.load(Ordering::Acquire));
    assert!(parser.lock().grid().pinned_run().is_some());
    assert_eq!(refusal(&app.mcp_run_start(McpControl::Agent, agent, "ls")), Some("busy"));
}

/// One of several leaves closes alone; a sole leaf closes its tab; the main window's last tab is
/// refused and nothing changes.
#[test]
fn close_pane_closes_exactly_what_it_names() {
    let (mut app, user, agent) = app_with_user_and_agent_panes();
    let created =
        app.mcp_create_pane(McpControl::Agent, agent, Placement::Right, Default::default());
    let LoopReply::Created(helper) = created else {
        panic!("helper should be created");
    };
    assert!(matches!(app.mcp_close_pane(McpControl::Agent, helper.pane_id), LoopReply::Closed));
    assert_eq!(app.main().unwrap().tab_states.len(), 2, "closing one leaf keeps the tab");
    assert!(app.main().unwrap().panes.contains_key(&agent));
    assert!(matches!(app.mcp_close_pane(McpControl::Agent, agent), LoopReply::Closed));
    assert_eq!(app.main().unwrap().tab_states.len(), 1, "a sole leaf closes its tab");
    assert_eq!(refusal(&app.mcp_close_pane(McpControl::Any, user)), Some("last_tab"));
    assert!(app.main().unwrap().panes.contains_key(&user));
}
