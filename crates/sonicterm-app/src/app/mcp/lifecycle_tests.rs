//! Pins the server's life inside the App: off by default, a reload that changes nothing keeps
//! the generation, a control change or disable revokes queued work, the loop answers resolved
//! panes with their handles, and stale, late or orphaned requests are dropped unrun.

use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::sync_channel;
use std::time::Duration;

use sonicterm_cfg::config::Config;
use sonicterm_cfg::keymap::Keymap;
use sonicterm_cfg::theme::Theme;
use sonicterm_vt::vt::CaptureStagingPool;

use super::*;
use crate::app::media::InlineMediaPool;

/// An App on private pools, as unit tests build one.
fn test_app() -> App {
    App::new(Theme::default(), Config::default(), Keymap::default())
        .with_capture_staging_pool(CaptureStagingPool::new())
        .with_inline_media_pool(InlineMediaPool::new())
}

/// Install a running server with `config` and a placeholder endpoint, without binding anything,
/// and make the App's config agree with it.
fn install_server(app: &mut App, config: McpConfig) -> Arc<McpShared> {
    let shared = McpShared::new(config.control, None);
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

/// The `[mcp]` values with the server on at `control`.
fn enabled(control: McpControl) -> McpConfig {
    McpConfig { enabled: true, control, ..McpConfig::default() }
}

/// A queued `ListPanes` request with this admitting `generation` and `deadline`.
fn request(
    generation: u64,
    deadline: Instant,
    closed: bool,
) -> (LoopRequest, std::sync::mpsc::Receiver<LoopReply>) {
    let (reply, answer) = sync_channel(1);
    let request = LoopRequest {
        call: LoopCall::ListPanes,
        generation,
        deadline,
        closed: Arc::new(AtomicBool::new(closed)),
        reply,
    };
    (request, answer)
}

/// The reply's refusal code, or `None` when the request ran.
fn refusal(reply: &LoopReply) -> Option<&'static str> {
    match reply {
        LoopReply::Refused(code, _) => Some(code),
        _ => None,
    }
}

/// With the default config nothing starts, and new panes get no control endpoint.
#[test]
fn the_server_is_off_by_default() {
    let mut app = test_app();
    assert!(!app.config.mcp.enabled);
    app.reconcile_mcp();
    assert!(app.services.mcp_server.is_none());
    assert!(app.mcp_endpoint().is_none());
}

/// A reload that leaves `[mcp]` alone keeps the server and its generation, so a running
/// `wait_for` admitted under it continues.
#[test]
fn an_unchanged_reload_keeps_the_generation() {
    let mut app = test_app();
    let shared = install_server(&mut app, enabled(McpControl::Agent));
    let before = shared.generation.load(Ordering::Acquire);
    app.reconcile_mcp();
    assert!(app
        .services
        .mcp_server
        .as_ref()
        .is_some_and(|server| Arc::ptr_eq(&server.shared, &shared)));
    assert_eq!(shared.generation.load(Ordering::Acquire), before);
}

/// A control change bumps the generation in place, so work admitted before it is dropped.
#[test]
fn a_control_change_revokes_queued_work() {
    let mut app = test_app();
    let shared = install_server(&mut app, enabled(McpControl::Agent));
    let admitted = shared.generation.load(Ordering::Acquire);
    app.config.mcp = enabled(McpControl::Off);
    app.reconcile_mcp();
    assert_eq!(shared.control(), McpControl::Off);
    assert_ne!(shared.generation.load(Ordering::Acquire), admitted);

    let (queued, answer) = request(admitted, Instant::now() + Duration::from_secs(5), false);
    shared.inbox.lock().push(queued);
    app.drain_mcp_requests();
    assert_eq!(refusal(&answer.try_recv().unwrap()), Some("revoked"));
}

/// Disabling drops the server and stops its shared state, which ends every connection.
#[test]
fn disabling_stops_the_server() {
    let mut app = test_app();
    let shared = install_server(&mut app, enabled(McpControl::Agent));
    app.config.mcp = McpConfig::default();
    app.reconcile_mcp();
    assert!(app.services.mcp_server.is_none());
    assert!(shared.is_stopped());
}

/// A stale generation, a passed deadline and a closed client each get `revoked` without the
/// loop doing the work.
#[test]
fn stale_late_or_orphaned_requests_are_dropped() {
    let mut app = test_app();
    app.__test_seed_tab("one");
    let shared = install_server(&mut app, enabled(McpControl::Agent));
    let current = shared.generation.load(Ordering::Acquire);
    let later = Instant::now() + Duration::from_secs(5);
    let cases = [
        ("stale generation", request(current - 1, later, false)),
        ("passed deadline", request(current, Instant::now() - Duration::from_millis(1), false)),
        ("closed client", request(current, later, true)),
    ];
    for (label, (queued, answer)) in cases {
        shared.inbox.lock().push(queued);
        app.drain_mcp_requests();
        assert_eq!(refusal(&answer.try_recv().unwrap()), Some("revoked"), "{label}");
    }
}

/// A connection's question reaches the loop through the inbox and comes back with the pane's
/// handles; the main pane of the active tab is the active one.
#[test]
fn the_loop_resolves_panes_for_a_connection() {
    let mut app = test_app();
    let first = app.__test_seed_tab("one");
    let second = app.__test_seed_tab("two");
    let shared = install_server(&mut app, enabled(McpControl::Agent));
    let asker = {
        let shared = Arc::clone(&shared);
        std::thread::spawn(move || {
            let call = super::super::CallScope::admit(&shared, &Arc::new(AtomicBool::new(false)));
            shared.ask(LoopCall::ListPanes, &call)
        })
    };
    // Without an event-loop proxy the test plays the loop: wait for the request, then drain.
    let deadline = Instant::now() + Duration::from_secs(5);
    while shared.inbox.lock().is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    app.drain_mcp_requests();
    let LoopReply::Panes(entries) = asker.join().unwrap() else {
        panic!("expected panes");
    };
    let ids: Vec<u64> = entries.iter().map(|entry| entry.pane_id).collect();
    assert_eq!(ids, vec![first, second]);
    assert_eq!(entries.iter().map(|entry| entry.tab_index).collect::<Vec<_>>(), vec![0, 1]);
    let active_tab = app.main_tabs().unwrap().active_index();
    assert!(entries.iter().all(|entry| entry.active == (entry.tab_index == active_tab)));
}

/// A request for a pane that does not exist is refused with `pane_gone`.
#[test]
fn a_missing_pane_is_gone() {
    let mut app = test_app();
    let shared = install_server(&mut app, enabled(McpControl::Agent));
    let (reply, answer) = sync_channel(1);
    shared.inbox.lock().push(LoopRequest {
        call: LoopCall::ResolvePane { pane_id: 999_999 },
        generation: shared.generation.load(Ordering::Acquire),
        deadline: std::time::Instant::now() + Duration::from_secs(5),
        closed: Arc::new(AtomicBool::new(false)),
        reply,
    });
    app.drain_mcp_requests();
    assert_eq!(refusal(&answer.try_recv().unwrap()), Some("pane_gone"));
}

/// Connection buffers are charged to a process-level `Mcp` owner below the root, and the owner
/// and its charge are gone once the server stops.
#[test]
fn connection_buffers_are_charged_to_the_mcp_owner() {
    let mut app = test_app();
    let shared = install_server(&mut app, enabled(McpControl::Agent));
    let owner = app
        .services
        .mcp_server
        .as_ref()
        .and_then(|server| server.owner.as_ref())
        .map(OwnerGuard::id);
    let owner = owner.expect("the owner registers");
    shared.hold_bytes(4096);
    shared.connections.store(1, Ordering::Release);
    app.charge_mcp_connections();
    let snapshot = app.governor.snapshot(owner).unwrap();
    assert_eq!(snapshot.owner_kind, OwnerKind::Mcp);
    assert_eq!(snapshot.owner_class_bytes[ResourceClass::ControlConnection], 4096);

    shared.release_bytes(4096);
    app.charge_mcp_connections();
    let snapshot = app.governor.snapshot(owner).unwrap();
    assert_eq!(snapshot.owner_class_bytes[ResourceClass::ControlConnection], 0);

    app.config.mcp = McpConfig::default();
    app.reconcile_mcp();
    assert!(app.governor.snapshot(owner).is_err(), "the owner outlived the server");
}

/// Every new pane gets its id; while a server runs it is also told where the server listens, and
/// without one it is not. The shell token is added by `arm_shell_token`, not here.
#[test]
fn new_panes_get_the_endpoint_while_a_server_runs() {
    let mut app = test_app();
    let names = |app: &App| -> Vec<String> {
        app.pane_control_env(5).into_iter().map(|(name, _)| name).collect()
    };
    assert_eq!(names(&app), vec!["SONICTERM_PANE_ID"]);
    install_server(&mut app, enabled(McpControl::Agent));
    assert_eq!(names(&app), vec!["SONICTERM_PANE_ID", "SONICTERM_CONTROL"]);
    assert!(app
        .pane_control_env(5)
        .contains(&("SONICTERM_CONTROL".to_owned(), "test-endpoint".to_owned())));
}

/// A connection notice queued by a connection thread is shown in the existing in-app
/// notification once the loop drains, with nobody waiting for a reply; a stopped server shows none.
#[test]
fn connection_notices_reach_the_notification() {
    let mut app = test_app();
    app.__test_seed_tab("one");
    let shared = install_server(&mut app, enabled(McpControl::Agent));
    shared.notify("Agent connected: test".into());
    app.drain_mcp_requests();
    let bubble = app.main().and_then(|main| main.notification.clone()).expect("a notification");
    assert_eq!(bubble.message, "Agent connected: test");
    app.main_mut().unwrap().notification = None;
    shared.stop();
    shared.notify("Agent disconnected: test".into());
    app.drain_mcp_requests();
    assert!(app.main().unwrap().notification.is_none());
}

/// A tool that names one pane resolves exactly the entry `list_panes` shows for it, without
/// enumerating every pane: across a main window with split tabs, two child windows, a report
/// window holding a pane-shaped entry, and a leaf whose pane is mid-teardown, the resolver agrees
/// with the list for every pane, and returns nothing for the report window, the torn-down leaf
/// and an unknown id.
#[test]
fn resolving_one_pane_matches_its_listed_entry() {
    let mut app = test_app();
    app.__test_seed_tab("one");
    app.__test_split_active_right();
    app.__test_seed_tab("two");
    app.__test_split_active_right();
    let first_child = app.__test_seed_child_window(&["c1", "c2"]);
    let second_child = app.__test_seed_child_window(&["d1"]);
    let report_child = app.__test_seed_child_window(&["report"]);
    let report_pane = app.__test_child_active_pane(report_child).unwrap();
    app.registry.windows.get_mut(&report_child).unwrap().role =
        crate::app::WindowRole::MemoryReport;
    // A terminal window created after the report window keeps its creation-order index: the
    // report window's slot is skipped, not reused (main 0, children 1, 2, report 3, this one 4).
    let after_report = app.__test_seed_child_window(&["after"]);
    let after_pane = app.__test_child_active_pane(after_report).unwrap();
    let first_child_panes: Vec<_> =
        (0..2).map(|tab| app.registry.windows[&first_child].tab_states[tab].active_pane).collect();
    let main_second_tab = app.main().unwrap().tab_states[1].active_pane;
    // A leaf whose pane already left `panes`, as during teardown, is listed by neither path.
    let torn = app.__test_child_active_pane(second_child).unwrap();
    let removed = app.registry.windows.get_mut(&second_child).unwrap().panes.remove(&torn);
    assert!(removed.is_some());
    let listed = app.mcp_pane_entries();
    assert!(
        listed.len() >= 5,
        "the layout has panes in several windows and tabs: {}",
        listed.len()
    );
    assert!(listed.iter().any(|entry| entry.window_index > 0), "child windows are listed");
    // Explicit (window, tab) indices, independent of the ordering helper both paths share.
    let expected = [
        (main_second_tab, 0, 1),
        (first_child_panes[0], 1, 0),
        (first_child_panes[1], 1, 1),
        (after_pane, 4, 0),
    ];
    for (pane, window_index, tab_index) in expected {
        let resolved = app.mcp_resolve_pane(pane).expect("a terminal pane resolves");
        assert_eq!(
            (resolved.window_index, resolved.tab_index),
            (window_index, tab_index),
            "pane {pane}"
        );
        let entry = listed.iter().find(|entry| entry.pane_id == pane).expect("listed");
        assert_eq!((entry.window_index, entry.tab_index), (window_index, tab_index), "pane {pane}");
    }
    for entry in &listed {
        let resolved = app.mcp_resolve_pane(entry.pane_id).expect("a listed pane resolves");
        let pair = |entry: &PaneEntry| {
            (entry.pane_id, entry.window_index, entry.tab_index, entry.active, entry.readonly)
        };
        assert_eq!(pair(&resolved), pair(entry), "pane {}", entry.pane_id);
        assert!(Arc::ptr_eq(&resolved.parser, &entry.parser), "the same pane's parser");
    }
    for absent in [report_pane, torn, 987_654] {
        assert!(app.mcp_resolve_pane(absent).is_none(), "pane {absent} is not a terminal pane");
        assert!(listed.iter().all(|entry| entry.pane_id != absent));
    }
}

/// Resolving one pane builds exactly one entry, where listing builds one per pane; counted while
/// resolving, so building every entry and discarding all but one (the old `find` over the list,
/// whose final reference counts look the same) fails here.
#[test]
fn resolving_one_pane_builds_only_its_entry() {
    let mut app = test_app();
    for title in ["one", "two", "three"] {
        app.__test_seed_tab(title);
        app.__test_split_active_right();
    }
    let built = || super::ENTRIES_BUILT.with(std::cell::Cell::get);
    let before_list = built();
    let listed = app.mcp_pane_entries();
    assert_eq!(built() - before_list, listed.len(), "the list builds one entry per pane");
    assert!(listed.len() >= 6);
    let target = listed[3].pane_id;
    let before_resolve = built();
    let resolved = app.mcp_resolve_pane(target).expect("a listed pane resolves");
    assert_eq!(built() - before_resolve, 1, "resolving builds only the named pane's entry");
    assert_eq!(resolved.pane_id, target);
}

/// A server running without `[mcp] read` shows the default-change notice once, only after a main
/// window exists; later calls show nothing more.
#[test]
fn an_unset_read_is_announced_once_after_a_window_exists() {
    let mut app = test_app();
    install_server(&mut app, enabled(McpControl::Agent));
    app.services.mcp_server.as_mut().unwrap().read_notice_due = true;
    // No main window yet: the notice waits.
    app.show_mcp_read_notice_if_due();
    assert!(app.services.mcp_server.as_ref().unwrap().read_notice_due);
    app.__test_seed_tab("first");
    app.show_mcp_read_notice_if_due();
    assert_eq!(app.__test_main_notification_message(), Some(READ_UNSET_NOTICE));
    assert!(!app.services.mcp_server.as_ref().unwrap().read_notice_due);
    app.main_mut().unwrap().notification = None;
    app.show_mcp_read_notice_if_due();
    assert_eq!(app.__test_main_notification_message(), None, "shown twice");
}

/// The notice names the actual default (agent-created panes, not "your own"), and an explicit
/// `read = "any"` is kept: only a missing key reads as `"agent"`.
#[test]
fn the_read_notice_wording_and_explicit_any() {
    assert!(READ_UNSET_NOTICE.contains("agent-created panes"));
    assert!(READ_UNSET_NOTICE.contains("read = \"any\""));
    let mut app = test_app();
    let mut config = enabled(McpControl::Agent);
    config.read = Some(sonicterm_cfg::config::McpRead::Any);
    let shared = install_server(&mut app, config);
    shared.set_read(config.read());
    assert_eq!(shared.read(), sonicterm_cfg::config::McpRead::Any);
}

/// The notice through the real lifecycle: a server started by `reconcile_mcp` without `read`
/// announces once when the first window appears; a reload that changes nothing says nothing more;
/// stopping and starting again (a restart) announces again; and an explicit `read` is never
/// announced, at start or on a restart.
#[cfg(unix)]
#[test]
fn the_read_notice_follows_start_reload_restart_and_explicit_keys() {
    // A fresh private directory the server creates itself, as it does its per-user one.
    let root = tempfile::tempdir().unwrap();
    let scratch = root.path().join("sonicterm");
    crate::app::mcp::endpoint_unix::TEST_ENDPOINT_DIR.set(Some(scratch.clone()));
    let mut app = test_app();
    let without_read = McpConfig { enabled: true, read: None, ..McpConfig::default() };
    app.config.mcp = without_read;
    app.reconcile_mcp();
    assert!(app.services.mcp_server.as_ref().expect("started").read_notice_due, "due at start");
    // The first main window shows it; the event loop calls this as that window appears.
    app.__test_seed_tab("first");
    app.show_mcp_read_notice_if_due();
    assert_eq!(app.__test_main_notification_message(), Some(READ_UNSET_NOTICE));
    // An unchanged reload keeps the server and says nothing more.
    app.main_mut().unwrap().notification = None;
    app.reconcile_mcp();
    app.show_mcp_read_notice_if_due();
    assert_eq!(app.__test_main_notification_message(), None, "a reload repeated it");
    // A restart (stop, then start without the key) announces again, at once since a window exists.
    app.config.mcp = McpConfig::default();
    app.reconcile_mcp();
    app.config.mcp = without_read;
    app.reconcile_mcp();
    assert_eq!(app.__test_main_notification_message(), Some(READ_UNSET_NOTICE));
    // An explicit read, at start or after a restart, is never announced.
    for read in [sonicterm_cfg::config::McpRead::Agent, sonicterm_cfg::config::McpRead::Any] {
        app.main_mut().unwrap().notification = None;
        app.config.mcp = McpConfig::default();
        app.reconcile_mcp();
        app.config.mcp = McpConfig { enabled: true, read: Some(read), ..McpConfig::default() };
        app.reconcile_mcp();
        assert!(!app.services.mcp_server.as_ref().unwrap().read_notice_due, "{read:?}");
        assert_eq!(app.__test_main_notification_message(), None, "{read:?} was announced");
    }
    app.config.mcp = McpConfig::default();
    app.reconcile_mcp();
    crate::app::mcp::endpoint_unix::TEST_ENDPOINT_DIR.set(None);
}

/// A pane outside the read scope is refused with the fix in the message itself, since a headless
/// agent never sees the notification.
#[test]
fn an_out_of_scope_refusal_names_the_fix() {
    let mut app = test_app();
    let pane_id = app.__test_seed_tab("user");
    let shared = install_server(&mut app, enabled(McpControl::Agent));
    shared.set_read(sonicterm_cfg::config::McpRead::Agent);
    let (reply, answer) = std::sync::mpsc::sync_channel(1);
    shared.inbox.lock().push(crate::app::mcp::LoopRequest {
        call: crate::app::mcp::LoopCall::ResolvePane { pane_id },
        generation: shared.generation.load(std::sync::atomic::Ordering::Acquire),
        deadline: std::time::Instant::now() + Duration::from_secs(5),
        closed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        reply,
    });
    app.drain_mcp_requests();
    match answer.recv().unwrap() {
        LoopReply::Refused(code, message) => {
            assert_eq!(code, "out_of_scope");
            assert!(message.contains("read = \"any\""), "{message}");
        }
        _ => panic!("the user's pane was not refused"),
    }
}
