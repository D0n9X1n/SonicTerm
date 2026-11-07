//! Pins agent pane targeting: a helper opens beside a pane in a background tab or a torn-out
//! window without moving the user's focus, a missing anchor changes nothing, READONLY in a
//! non-frontmost window refuses agent input and helpers, and the pane names itself to its child.

use sonicterm_ui::copy_mode::CopyModeState;
use sonicterm_ui::pane::Rect;

use super::*;

/// An app whose configured shell is a path below the test executable, so no shell launches and
/// no user profile is read; panes are installed without a PTY.
fn app_with_unavailable_shell() -> App {
    let shell = std::env::current_exe().expect("test executable").join("unavailable-shell");
    let config = Config {
        terminal: sonicterm_cfg::config::TerminalConfig {
            shell: Some(shell.to_string_lossy().into_owned()),
            ..Default::default()
        },
        ..Config::default()
    };
    App::new(Theme::default(), config, Keymap::default())
}

/// An app whose shell cannot launch, with two main tabs (`anchor` in the background one, the
/// second active) and a 10 x 10 px headless main viewport.
fn app_with_background_anchor() -> (App, u64, u64) {
    let mut app = app_with_unavailable_shell();
    let anchor = app.__test_seed_tab("background");
    let active = app.__test_seed_tab("front");
    assert!(app.__test_set_main_pane_viewport(Rect::new(0.0, 0.0, 800.0, 240.0), 10.0, 10.0));
    (app, anchor, active)
}

/// A split beside a pane in the background tab lands in that tab, keeps the active tab and pane,
/// is agent-owned, and is sized from that tab's own layout rather than the spawn size.
#[test]
fn a_split_in_a_background_tab_keeps_focus_and_takes_its_own_size() {
    let (mut app, anchor, active) = app_with_background_anchor();
    let created = app
        .create_pane_at(anchor, Placement::Right, AgentLaunch::default())
        .expect("helper created");
    assert_eq!(created.launch, LaunchOutcome::Failed, "the test shell cannot launch");
    let window = app.main().expect("main");
    assert_eq!(window.tabs.active_index(), 1, "the active tab did not change");
    assert_eq!(window.tab_states[1].active_pane, active);
    assert_eq!(window.tab_states[0].active_pane, anchor, "the anchor keeps its tab's focus");
    assert!(window.tab_states[0].tree.leaves().contains(&created.pane_id));
    assert!(window.panes[&created.pane_id].agent_owned);
    // The background tab's 800 px is split in two, so each 10 px-cell pane is 40 columns wide.
    let grid_cols = window.panes[&created.pane_id].parser.lock().grid().cols;
    assert_eq!(grid_cols, 40, "sized from the hidden tab's layout, not the 80-column spawn size");
}

/// A new tab in the anchor's window is appended without activating it, and named as asked.
#[test]
fn a_new_tab_is_appended_without_activation() {
    let (mut app, anchor, _active) = app_with_background_anchor();
    let launch = AgentLaunch { title: Some("dev server".into()), ..AgentLaunch::default() };
    let created = app.create_pane_at(anchor, Placement::Tab, launch).expect("tab created");
    let window = app.main().expect("main");
    assert_eq!(window.tabs.len(), 3);
    assert_eq!(window.tabs.active_index(), 1, "the user's tab stays active");
    assert_eq!(window.tab_states[2].active_pane, created.pane_id);
    assert!(window.tabs.tabs()[2].title.contains("dev server"));
}

/// A split beside a pane in a torn-out child window lands in that child, not the main window.
#[test]
fn a_split_beside_a_torn_out_pane_stays_in_its_window() {
    let (mut app, _anchor, _active) = app_with_background_anchor();
    let child = app.__test_seed_child_window(&["child"]);
    assert!(app.__test_set_child_pane_viewport(
        child,
        Rect::new(0.0, 0.0, 400.0, 200.0),
        10.0,
        10.0
    ));
    let child_pane = app.registry.windows[&child].tab_states[0].active_pane;
    let created = app
        .create_pane_at(child_pane, Placement::Down, AgentLaunch::default())
        .expect("helper created");
    assert_eq!(
        app.locate_pane(created.pane_id).map(|location| location.window_id),
        Some(child),
        "the helper lives in the anchor's window"
    );
    assert_eq!(app.registry.windows[&child].tab_states[0].active_pane, child_pane, "focus stays");
}

/// A closed anchor fails with `PaneGone` and leaves every window's panes as they were.
#[test]
fn a_missing_anchor_changes_nothing() {
    let (mut app, anchor, _active) = app_with_background_anchor();
    let before = app.main().expect("main").panes.len();
    assert_eq!(
        app.create_pane_at(anchor + 1_000, Placement::Right, AgentLaunch::default()),
        Err(TargetError::PaneGone)
    );
    assert_eq!(app.main().expect("main").panes.len(), before);
    assert_eq!(
        app.write_agent_input(anchor + 1_000, b"x".to_vec(), PtyInputSource::Agent),
        Err(TargetError::PaneGone)
    );
}

/// READONLY in a window that is not frontmost refuses both agent input to its pane and a new
/// helper anchored there; nothing is queued and no pane is added.
#[test]
fn readonly_in_a_background_window_refuses_input_and_helpers() {
    let (mut app, _anchor, _active) = app_with_background_anchor();
    let child = app.__test_seed_child_window(&["child"]);
    let child_pane = app.registry.windows[&child].tab_states[0].active_pane;
    app.registry.windows.get_mut(&child).expect("child").copy_mode =
        Some(CopyModeState::read_only_at((0, 0)));
    app.testing.pty_write_log_enabled = true;
    assert_eq!(
        app.write_agent_input(child_pane, b"rm -rf\r".to_vec(), PtyInputSource::Agent),
        Err(TargetError::ReadOnly)
    );
    assert!(app.testing.test_pty_writes.lock().is_empty(), "nothing was queued");
    assert_eq!(
        app.create_pane_at(child_pane, Placement::Right, AgentLaunch::default()),
        Err(TargetError::ReadOnly)
    );
    assert_eq!(app.registry.windows[&child].panes.len(), 1);
}

/// A program the agent names runs held, so its exit status and output stay; the pane's spawn
/// options name it to its child; and a program that cannot start is reported as failed with the
/// pane still installed.
#[test]
fn an_agent_launch_is_held_and_names_its_pane() {
    let (mut app, anchor, _active) = app_with_background_anchor();
    let launch = AgentLaunch {
        argv: vec!["/nonexistent/sonicterm-agent-helper".into(), "--flag".into()],
        ..AgentLaunch::default()
    };
    let created = app.create_pane_at(anchor, Placement::Right, launch).expect("installed");
    assert_eq!(created.launch, LaunchOutcome::Failed, "the program cannot start");
    let pane = &app.main().expect("main").panes[&created.pane_id];
    assert!(pane.hold_on_exit, "an agent program is held after it exits");
    let launches = app.testing.test_pane_launches.borrow();
    let (_, recorded) = launches.iter().find(|(id, _)| *id == created.pane_id).expect("launched");
    assert!(recorded.command.is_some() && recorded.hold);
    let opts = app.pane_spawn_opts(created.pane_id, recorded, false);
    assert_eq!(
        opts.extra_env.first(),
        Some(&("SONICTERM_PANE_ID".to_owned(), created.pane_id.to_string())),
        "the spawn options handed to the PTY carry the pane id"
    );
}

/// The exit status is recorded once, before the hold path's repeat check, so a waiter reads the
/// first status and a second report cannot replace it.
#[test]
fn the_exit_status_is_set_once() {
    let (mut app, anchor, _active) = app_with_background_anchor();
    let cell = Arc::clone(&app.main().expect("main").panes[&anchor].exit_status);
    app.main_mut().expect("main").panes.get_mut(&anchor).expect("pane").hold_on_exit = true;
    app.handle_pane_process_exited(anchor, sonicterm_types::PaneExitStatus::Code(7));
    app.handle_pane_process_exited(anchor, sonicterm_types::PaneExitStatus::Code(0));
    assert_eq!(cell.get(), Some(&sonicterm_types::PaneExitStatus::Code(7)));
}

/// The agent-owned mark lives on the pane, so it survives moving the pane's tab to another
/// window.
#[test]
fn agent_ownership_survives_a_tab_transfer() {
    let (mut app, anchor, _active) = app_with_background_anchor();
    let created =
        app.create_pane_at(anchor, Placement::Tab, AgentLaunch::default()).expect("tab created");
    let child = app.__test_seed_child_window(&["child"]);
    // A destination needs presentation geometry before it accepts a tab.
    assert!(app.__test_set_child_pane_viewport(
        child,
        Rect::new(0.0, 0.0, 400.0, 200.0),
        10.0,
        10.0
    ));
    app.transfer_tab(None, 2, Some(child), 1).expect("the helper tab moves");
    let location = app.locate_pane(created.pane_id).expect("still live");
    assert_eq!(location.window_id, child);
    assert!(app.registry.windows[&child].panes[&created.pane_id].agent_owned);
}

/// The helper's grid starts at its split size, so a program that reads its size at launch never
/// sees the whole-window size; an `argv` launch inherits the anchor's trusted directory.
#[test]
fn a_helper_starts_at_its_split_size_and_inherits_the_anchor_cwd() {
    let (mut app, anchor, _active) = app_with_background_anchor();
    // A file URI with a drive prefix on Windows, as a shell there reports it.
    let prefix = if cfg!(windows) { "/C:" } else { "" };
    let osc7 = format!("\x1b]7;file://localhost{prefix}/work\x07");
    let dir = std::path::PathBuf::from(if cfg!(windows) { r"C:\work" } else { "/work" });
    app.main().expect("main").panes[&anchor].parser.lock().advance(osc7.as_bytes());
    let launch = AgentLaunch { argv: vec!["/nonexistent/helper".into()], ..AgentLaunch::default() };
    let location = app.locate_pane(anchor).expect("anchor");
    // The split gives each pane 400 of the 800 px at 10 px cells: 40 x 24.
    assert_eq!(app.target_cells(location, anchor, Placement::Right, u64::MAX), Some((40, 24)));
    let created = app.create_pane_at(anchor, Placement::Right, launch).expect("installed");
    let launches = app.testing.test_pane_launches.borrow();
    let (_, recorded) = launches.iter().find(|(id, _)| *id == created.pane_id).expect("launched");
    assert_eq!(recorded.cwd.as_deref(), Some(dir.as_path()), "the anchor's directory is inherited");
}

/// A helper in a hidden tab of a child window gets a memory owner at once, like a visible one.
#[test]
fn a_hidden_child_tab_helper_is_owned_at_once() {
    let (mut app, _anchor, _active) = app_with_background_anchor();
    let child = app.__test_seed_child_window(&["first", "second"]);
    assert!(app.__test_set_child_pane_viewport(
        child,
        Rect::new(0.0, 0.0, 400.0, 200.0),
        10.0,
        10.0
    ));
    let hidden = app.registry.windows[&child].tab_states[0].active_pane;
    let created = app
        .create_pane_at(hidden, Placement::Right, AgentLaunch::default())
        .expect("helper created");
    let window = &app.registry.windows[&child];
    assert_ne!(window.tabs.active_index(), 0, "the anchor's tab is hidden");
    assert_eq!(
        window.panes[&created.pane_id].owner.is_some(),
        window.panes[&hidden].owner.is_some(),
        "the helper is registered like the panes already there"
    );
}
