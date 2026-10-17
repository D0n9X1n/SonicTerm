use sonicterm_cfg::keymap::{Action, BroadcastScope, Keymap, ScrollAction};
use sonicterm_cfg::{config::Config, theme::Theme};
use sonicterm_ui::selection::Selection;

use super::*;

/// A headless app with one main tab named `zsh` and an open report window.
fn app_with_report() -> (App, u64, WindowId) {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.test_clipboard_text = Some(String::new());
    let main_pane = app.__test_seed_tab("zsh");
    let report_id = app.__test_open_memory_report_window();
    (app, main_pane, report_id)
}

/// The report pane's id.
fn report_pane(app: &App, report_id: WindowId) -> u64 {
    let window = &app.windows[&report_id];
    window.tab_states[window.tabs.active_index()].active_pane
}

/// Contract: running Memory Usage while a report is open focuses and refreshes that one window
/// instead of opening a second, and with no report open it only requests one.
#[test]
fn showing_memory_usage_twice_keeps_one_window() {
    let (mut app, _, report_id) = app_with_report();
    let first_generation = app.memory_report.generation;
    assert!(app.run_action(&Action::ShowMemoryUsage));
    assert!(app.run_action(&Action::ShowMemoryUsage));
    let reports =
        app.windows.values().filter(|window| window.role == WindowRole::MemoryReport).count();
    assert_eq!(reports, 1);
    assert_eq!(app.memory_report_window_id(), Some(report_id));
    assert!(app.memory_report.generation > first_generation, "a repeat run refreshes");
    assert!(!app.memory_report.open_requested);

    let mut fresh = App::new(Theme::default(), Config::default(), Keymap::default());
    fresh.__test_seed_tab("zsh");
    assert!(fresh.run_action(&Action::ShowMemoryUsage));
    assert!(fresh.memory_report.open_requested, "no window yet: the event loop creates it");
}

/// Contract: the report pane shows exactly the rendered report, and copy-all puts exactly that
/// text on the clipboard.
#[test]
fn copy_all_copies_exactly_the_rendered_report() {
    let (mut app, _, report_id) = app_with_report();
    let rendered = app.memory_report.current.as_ref().expect("report").render();
    assert_eq!(app.memory_report.last_text.as_deref(), Some(rendered.as_str()));
    assert!(rendered.starts_with("SonicTerm "), "{rendered}");
    assert!(rendered.contains("PROCESS (from the OS)"));
    assert!(rendered.contains("\"zsh\""));
    app.frontmost_window = Some(report_id);
    assert!(app.run_action(&Action::CopyMemoryReport));
    assert_eq!(app.test_clipboard_text.as_deref(), Some(rendered.as_str()));
    let parser = app.windows[&report_id].panes[&report_pane(&app, report_id)].parser.clone();
    let first_row = {
        let guard = parser.lock();
        let mut selection = Selection::new(0, 0);
        selection.extend(0, 9);
        selection.as_text(guard.grid())
    };
    assert!(first_row.starts_with("SonicTerm"), "{first_row}");
}

/// Contract: the report pane has no PTY, so a paste or typed key aimed at it writes nothing to
/// any shell, including the main window's.
#[test]
fn keys_and_pastes_in_the_report_never_reach_a_pty() {
    let (mut app, _, report_id) = app_with_report();
    assert!(app.windows[&report_id].panes.values().all(|pane| pane.pty.is_none()));
    app.__test_enable_pty_write_log();
    app.test_clipboard_text = Some("rm -rf /\n".to_string());
    assert!(app.run_action_for_window(&Action::PasteFromClipboard, report_id));
    app.handle_memory_report_key(report_id, &["x".to_string()], false);
    assert!(app.__test_pty_write_log().is_empty());
}

/// Contract: Copy in the report window copies the report's own selection, not the main window's.
#[test]
fn copy_in_the_report_takes_the_report_selection() {
    let (mut app, main_pane, report_id) = app_with_report();
    let main_parser = app.main().unwrap().panes[&main_pane].parser.clone();
    main_parser.lock().advance(b"main text");
    let pane_id = report_pane(&app, report_id);
    let parser = app.windows[&report_id].panes[&pane_id].parser.clone();
    let (selection, expected) = {
        let guard = parser.lock();
        let grid = guard.grid();
        let mut selection = Selection::new(0, 0);
        selection.extend(0, 8);
        let selection = selection
            .with_content_state(
                pane_id,
                grid.content_seq(),
                grid.is_alt(),
                grid.scrollback_evicted(),
            )
            .with_content_fingerprint(grid);
        let expected = selection.as_text(grid);
        (selection, expected)
    };
    assert!(!expected.is_empty());
    app.windows.get_mut(&report_id).unwrap().selection = Some(selection);
    app.frontmost_window = Some(report_id);
    assert!(app.run_action(&Action::CopyToClipboard));
    assert_eq!(app.test_clipboard_text.as_deref(), Some(expected.as_str()));
    assert!(!expected.contains("main text"));
}

/// Contract: window-level actions from the report run in the main terminal window, and pane
/// structure actions from it change nothing anywhere.
#[test]
fn report_actions_redirect_or_are_consumed() {
    let (mut app, _, report_id) = app_with_report();
    assert!(app.run_action_for_window(&Action::NewTab, report_id));
    assert_eq!(app.__test_main_tab_count(), 2, "NewTab ran in the main window");
    assert_eq!(app.windows[&report_id].tabs.len(), 1);
    app.frontmost_window = Some(report_id);
    assert!(app.run_action(&Action::SplitRight));
    assert!(app.run_action(&Action::NextTab));
    assert_eq!(app.windows[&report_id].panes.len(), 1, "the report pane is never split");
    assert_eq!(app.__test_main_tab_count(), 2);
    assert!(app.run_action(&Action::Scroll(ScrollAction::PageUp)));
}

/// Contract: closing the report window never quits, and leaves the terminal window untouched.
#[test]
fn closing_the_report_never_quits() {
    let (mut app, _, report_id) = app_with_report();
    assert!(app.run_action_for_window(&Action::CloseActivePaneOrTab, report_id));
    assert!(app.memory_report_window_id().is_none());
    assert!(!app.should_exit());
    assert!(!app.pending_exit);
    assert_eq!(app.__test_main_tab_count(), 1);
    assert!(app.memory_report.last_text.is_none(), "no stale text survives the window");
}

/// Contract: the report window does not count as a terminal window, so closing the last
/// terminal tab while it is open still exits the app.
#[test]
fn closing_the_last_terminal_exits_with_the_report_open() {
    let (mut app, _, report_id) = app_with_report();
    assert_eq!(app.child_window_count(), 0);
    app.frontmost_window = app.main_window_id;
    assert!(app.run_action(&Action::CloseTab));
    assert!(app.windows.contains_key(&report_id));
    assert!(app.should_exit());
    assert!(app.pending_exit, "the last terminal close requests exit despite the report");
}

/// Contract: a broadcast to all tabs never includes the report pane as a receiver.
#[test]
fn broadcast_never_targets_the_report_pane() {
    let (mut app, main_pane, report_id) = app_with_report();
    let second = app.__test_seed_tab("second");
    app.frontmost_window = app.main_window_id;
    app.__test_set_active_pane(0, main_pane);
    app.toggle_broadcast(BroadcastScope::AllTabs);
    let receivers = app.__test_broadcast_receivers();
    assert!(!receivers.is_empty(), "the other main pane receives");
    assert!(receivers.iter().all(|pane| *pane == main_pane || *pane == second), "{receivers:?}");
    assert!(!receivers.contains(&report_pane(&app, report_id)), "{receivers:?}");
}

/// Contract: the report's per-pane rows sum to the session total of the same sampling cycle, so
/// the report and the `memory snapshot` log line agree.
#[test]
fn per_pane_rows_sum_to_the_session_total() {
    let (mut app, _, _) = app_with_report();
    app.__test_seed_tab("second");
    let report = app.collect_memory_report();
    let summed: usize = report
        .panes
        .iter()
        .filter_map(|pane| pane.retention.map(|retention| retention.total().bytes))
        .sum();
    assert_eq!(summed, report.snapshot.session_bytes());
    assert_eq!(report.panes.len(), report.snapshot.panes_total);
    assert!(report.panes.iter().any(|pane| pane.is_report));
}

/// Contract: the report pane keeps its own history, so a small `[terminal] scrollback` does not
/// trim a long report.
#[test]
fn report_history_is_independent_of_the_scrollback_setting() {
    let mut config = Config::default();
    config.terminal.scrollback = 5;
    let mut app = App::new(Theme::default(), config, Keymap::default());
    app.__test_seed_tab("zsh");
    let report_id = app.__test_open_memory_report_window();
    let long_report: String = (0..300).map(|line| format!("line {line}\n")).collect();
    assert!(app.feed_memory_report(&long_report));
    let parser = app.windows[&report_id].panes[&report_pane(&app, report_id)].parser.clone();
    let history = parser.lock().grid().scrollback_len();
    assert!(history > 5, "history {history} was trimmed to the user setting");
}

/// Contract: an OS sample for an older refresh is dropped, so a slow walk cannot overwrite a
/// newer report.
#[test]
fn a_stale_os_sample_is_dropped() {
    let (mut app, _, _) = app_with_report();
    let before = app.memory_report.last_text.clone();
    let stale = app.memory_report.generation.wrapping_sub(1);
    let sample = OsSample {
        footprint: sonicterm_logging::process_report::sample_footprint(),
        uptime_secs: Some(1),
        threads: sonicterm_logging::process_report::ThreadSummary {
            total: Some(999),
            names: Vec::new(),
        },
        trees: Vec::new(),
    };
    app.handle_memory_report_sampled(stale, sample);
    assert_eq!(app.memory_report.last_text, before);
}

/// Contract: IME composition, search and copy mode own keys before the report window does, so
/// R and Esc reach them first; the report branch sits only inside the terminal owner's arm.
#[test]
fn report_keys_run_only_after_composition_search_and_copy_mode() {
    let source = include_str!("window_keyboard.rs");
    let terminal_arm = source.find("Some(WindowKeyOwner::Terminal) =>").expect("terminal arm");
    let report_branch = source.find("self.handle_memory_report_key(").expect("report branch");
    for earlier in [
        "Some(WindowKeyOwner::Composition) =>",
        "Some(WindowKeyOwner::Search) =>",
        "Some(WindowKeyOwner::Copy) =>",
    ] {
        assert!(source.find(earlier).expect(earlier) < terminal_arm, "{earlier}");
    }
    assert!(terminal_arm < report_branch);
}
