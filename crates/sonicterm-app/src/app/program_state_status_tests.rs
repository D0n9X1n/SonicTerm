//! Pins the OSC 7501 tab marker and lifetimes in the App: an inactive tab's marker updates from
//! its own panes, urgency then last change ranks panes, a stale summary never replaces a newer
//! one, a held exit ends transient records, a real key and focus acknowledge done and error while
//! agent input does not, and a discarded report logs its reason, never its payload.

use std::sync::Arc;

use parking_lot::Mutex;
use sonicterm_cfg::config::Config;
use sonicterm_cfg::keymap::Keymap;
use sonicterm_cfg::theme::Theme;
use sonicterm_ui::tabs::StatusMarker;
use sonicterm_vt::vt::VtEvent;
use winit::window::WindowId;

use super::*;
use crate::app::input_dispatch::PtyInputSource;

/// Feed `bytes` to `pane_id`'s parser and hand the batch's summary to the App, as the VT worker
/// and the event loop do.
fn feed(app: &mut App, pane_id: u64, bytes: &str) {
    let summary = {
        let pane = app.registry.pane_mut(pane_id).expect("live pane");
        let events = pane.parser.lock().advance(bytes.as_bytes());
        events.into_iter().rev().find_map(|event| match event {
            VtEvent::ProgramStatus(summary) => Some(summary),
            _ => None,
        })
    };
    if let Some(summary) = summary {
        app.note_program_status(pane_id, summary);
    }
}

/// Refresh `window`'s tab markers as a frame does and return them in tab order.
fn markers(app: &mut App, window: WindowId) -> Vec<Option<StatusMarker>> {
    let state = app.registry.windows.get_mut(&window).expect("window");
    crate::app::privilege::refresh_window_agent_tabs(
        &mut state.tabs,
        &state.tab_states,
        &state.panes,
    );
    state.tabs.tabs().iter().map(|tab| tab.status).collect()
}

/// A child window of two tabs, `back` active, and its panes in tab order.
fn two_tabs() -> (App, WindowId, u64, u64) {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let child = app.__test_seed_child_window(&["front", "back"]);
    let panes: Vec<u64> =
        app.registry.windows[&child].tab_states.iter().map(|tab| tab.active_pane).collect();
    (app, child, panes[0], panes[1])
}

/// A report from a pane in an inactive tab marks that tab, not the active one, and the marker is
/// separate chrome: the tab's title text is unchanged.
#[test]
fn an_inactive_tab_shows_its_own_panes_marker() {
    let (mut app, child, front, _) = two_tabs();
    let title_before = app.registry.windows[&child].tabs.tabs()[0].title.clone();
    feed(&mut app, front, "\x1b]7501;app=claude-code:state=blocked:kind=permission\x07");
    assert_eq!(markers(&mut app, child), vec![Some(StatusMarker::Blocked), None]);
    assert_eq!(app.registry.windows[&child].tabs.tabs()[0].title, title_before);
}

/// Across the panes of one tab the most urgent record wins and ties go to the most recent change;
/// an identical report is no change, so it never takes the tie.
#[test]
fn urgency_then_last_change_ranks_a_tabs_panes() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let first = app.__test_seed_tab("split");
    app.__test_set_main_pane_viewport(
        sonicterm_ui::pane::Rect::new(0.0, 0.0, 800.0, 240.0),
        10.0,
        10.0,
    );
    app.__test_split_active_right();
    let main = app.registry.main_window_id.expect("main window");
    let second = app.registry.windows[&main].tab_states[0].active_pane;
    feed(&mut app, first, "\x1b]7501;state=working:progress=10\x07");
    feed(&mut app, second, "\x1b]7501;state=working:progress=20\x07");
    assert_eq!(markers(&mut app, main), vec![Some(StatusMarker::Working(Some(20)))]);
    feed(&mut app, first, "\x1b]7501;state=working:progress=10\x07");
    assert_eq!(markers(&mut app, main), vec![Some(StatusMarker::Working(Some(20)))]);
    feed(&mut app, first, "\x1b]7501;state=working:progress=30\x07");
    assert_eq!(markers(&mut app, main), vec![Some(StatusMarker::Working(Some(30)))]);
    feed(&mut app, second, "\x1b]7501;state=done\x07");
    assert_eq!(markers(&mut app, main), vec![Some(StatusMarker::Done)]);
    feed(&mut app, first, "\x1b]7501;state=error\x07");
    assert_eq!(markers(&mut app, main), vec![Some(StatusMarker::Error)]);
    feed(&mut app, second, "\x1b]7501;id=q:state=blocked\x07");
    assert_eq!(markers(&mut app, main), vec![Some(StatusMarker::Blocked)]);
}

/// A summary older than the stored one (a worker event that arrives after an acknowledgment)
/// never brings back a record the App already ended.
#[test]
fn a_stale_summary_never_replaces_a_newer_one() {
    let (mut app, child, front, _) = two_tabs();
    let stale = {
        let pane = app.registry.pane_mut(front).unwrap();
        pane.parser.lock().advance(b"\x1b]7501;state=done\x07");
        pane.parser.lock().program_status_summary()
    };
    app.note_program_status(front, stale);
    app.registry.pane_mut(front).unwrap().acknowledge_program_status();
    app.note_program_status(front, stale);
    assert_eq!(markers(&mut app, child), vec![None, None]);
}

/// A held, failed exit ends working, blocked and idle records and keeps done and error, so the
/// marker of the exited pane's tab follows at once.
#[test]
fn a_held_failed_exit_ends_transient_records() {
    let (mut app, child, front, _) = two_tabs();
    feed(&mut app, front, "\x1b]7501;id=a:state=blocked\x07\x1b]7501;id=b:state=done\x07");
    assert_eq!(markers(&mut app, child)[0], Some(StatusMarker::Blocked));
    app.handle_pane_process_exited(front, sonicterm_types::PaneExitStatus::Code(1));
    assert_eq!(markers(&mut app, child)[0], Some(StatusMarker::Done));
    feed(&mut app, front, "\x1b]7501;id=c:state=working\x07");
    app.handle_pane_process_exited(
        front,
        sonicterm_types::PaneExitStatus::Signal("SIGKILL".into()),
    );
    assert_eq!(markers(&mut app, child)[0], Some(StatusMarker::Done), "a repeated exit report");
}

/// A key the user types acknowledges the pane's done and error records; agent text and terminal
/// replies do not, and the status itself never writes input to the pane.
#[test]
fn a_real_key_acknowledges_and_agent_input_does_not() {
    let (mut app, child, front, _) = two_tabs();
    app.testing.pty_write_log_enabled = true;
    feed(&mut app, front, "\x1b]7501;state=error\x07");
    assert!(app.testing.test_pty_writes.lock().is_empty(), "a status writes nothing to the pane");
    for source in [PtyInputSource::Agent, PtyInputSource::AgentRun, PtyInputSource::TerminalReply] {
        app.write_to_pane(front, b"x".to_vec(), source);
        assert_eq!(markers(&mut app, child)[0], Some(StatusMarker::Error), "{source:?}");
    }
    app.dispatch_terminal_key_writes(vec![(
        front,
        crate::app::keyboard_protocol::EncodedKey {
            bytes: b"x".to_vec(),
            held: crate::app::keyboard_protocol::HeldKey::Legacy,
        },
    )]);
    assert_eq!(markers(&mut app, child)[0], None);
}

/// Focus acknowledges on its edge: the active pane of the frontmost window sees its done and
/// error records end once when it gains focus (native window focus or a tab switch), a report
/// that arrives while it stays focused waits for a key, and an unfocused window acknowledges
/// nothing. Nothing here focuses a window or a tab by itself.
#[test]
fn focus_acknowledges_on_its_edge_only() {
    let (mut app, child, front, back) = two_tabs();
    feed(&mut app, back, "\x1b]7501;state=done\x07");
    feed(&mut app, front, "\x1b]7501;state=error\x07");
    app.acknowledge_focused_status_in(child);
    assert_eq!(markers(&mut app, child), vec![Some(StatusMarker::Error), Some(StatusMarker::Done)]);
    assert_eq!(app.registry.frontmost_window, None, "a status never focuses a window");
    app.handle_window_focus_changed(child, true);
    app.acknowledge_focused_status_in(child);
    assert_eq!(markers(&mut app, child), vec![Some(StatusMarker::Error), None]);
    feed(&mut app, back, "\x1b]7501;state=done\x07");
    app.acknowledge_focused_status_in(child);
    assert_eq!(markers(&mut app, child)[1], Some(StatusMarker::Done), "no new focus edge");
    app.registry.windows.get_mut(&child).unwrap().tabs.activate(0);
    app.acknowledge_focused_status_in(child);
    assert_eq!(markers(&mut app, child), vec![None, Some(StatusMarker::Done)]);
    app.handle_window_focus_changed(child, false);
    app.acknowledge_focused_status_in(child);
    app.registry.windows.get_mut(&child).unwrap().tabs.activate(1);
    app.handle_window_focus_changed(child, true);
    app.acknowledge_focused_status_in(child);
    assert_eq!(markers(&mut app, child), vec![None, None], "refocus is a new edge");
}

/// Both redraw paths acknowledge focus before they refresh the tab markers.
#[test]
fn both_redraw_paths_acknowledge_focus_before_refreshing_markers() {
    for (name, source) in [
        ("main", include_str!("window_event.rs")),
        ("child", include_str!("child_window_redraw.rs")),
    ] {
        let acknowledged = source.find("acknowledge_focused_status_in(").expect(name);
        let refreshed = source.find("refresh_window_tab_privileges(").expect(name);
        assert!(acknowledged < refreshed, "{name} acknowledges after refreshing");
    }
}

/// A shared in-memory log sink.
#[derive(Clone, Default)]
struct LogSink(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for LogSink {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A discarded report logs a fixed reason code and never its id, app, state, title or message,
/// and an accepted one logs nothing of its payload either.
#[test]
fn logs_carry_no_status_payload() {
    let sink = LogSink::default();
    let writer = sink.clone();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_max_level(tracing::Level::TRACE)
        .with_writer(move || writer.clone())
        .finish();
    sonicterm_logging::test_capture::with_default(subscriber, || {
        let (mut app, _, front, _) = two_tabs();
        feed(&mut app, front, "\x1b]7501;id=secretid:app=secretapp:state=secretstate\x07");
        feed(&mut app, front, "\x1b]7501;id=secretid:app=secretapp:state=done:title=c2VjcmV0\x07");
    });
    let output = String::from_utf8(sink.0.lock().clone()).unwrap();
    assert!(output.contains("OSC 7501 report discarded"), "{output}");
    assert!(!output.contains("secret"), "payload logged: {output}");
}
