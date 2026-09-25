use super::*;

#[cfg(any(windows, unix))]
use crate::app::{
    mod_tests::{attach_idle_pty, input_test_windows, InputTestApp, PtySubmissions},
    pty_test_support::isolated,
};
#[cfg(any(windows, unix))]
use sonicterm_cfg::keymap::BroadcastScope;
#[cfg(any(windows, unix))]
use sonicterm_ui::{broadcast::BroadcastState, copy_mode::CopyModeState, search::SearchState};

/// Search owns explicit-source clipboard paste before READONLY, without changing another tab, window, or PTY.
#[cfg(any(windows, unix))]
#[test]
fn real_pty_search_paste_explicit_source_matrix() {
    if isolated() {
        return;
    }
    for source_index in 0..3 {
        let (mut app, windows) = search_paste_fixture();
        let (source_window, source) = windows[source_index];
        let (other_window, bracketed) = windows[(source_index + 1) % 3];
        let (_, peer) = windows[(source_index + 2) % 3];
        let inactive = next_pane_id();
        let parser = Arc::new(Mutex::new(Parser::new(Grid::new(40, 3))));
        let mut inactive_tab = TabState::new(PaneTree::leaf(inactive), inactive);
        let mut inactive_search = SearchState::new();
        inactive_search.set_query("inactive", parser.lock().grid());
        inactive_tab.search = Some(inactive_search);
        let window = app.windows.get_mut(&source_window).unwrap();
        window.panes.insert(inactive, PaneState::new(parser, None));
        window.tabs.insert(0, Tab::new("inactive"));
        window.tab_states.insert(0, inactive_tab);
        window.tabs.activate(1);
        attach_idle_pty(&mut app, source_window, inactive);
        app.pane_by_id(bracketed).unwrap().parser.lock().advance(b"\x1b[?2004h");
        app.broadcast = BroadcastState::On { scope: BroadcastScope::AllTabs, source_pane: source };
        app.frontmost_window = Some(other_window);
        app.__test_set_memory_clipboard("le");
        let submitted = PtySubmissions::start();

        // Writable search, silent READONLY refusal, and guarded terminal delivery are baseline controls.
        for (read_only, search_open) in [(false, true), (true, false), (false, false), (true, true)]
        {
            set_search_paste_query(&mut app, source_window, "need");
            let window = app.windows.get_mut(&source_window).unwrap();
            window.copy_mode = read_only.then(|| CopyModeState::read_only_at((0, 0)));
            if !search_open {
                window.tab_states[1].search = None;
            }
            let modes: Vec<_> =
                windows.iter().map(|(id, _)| app.windows[id].copy_mode.clone()).collect();
            app.wait_for_input_queues();
            assert!(app.run_action_for_window(&Action::PasteFromClipboard, source_window));
            let attempts = app.__test_drain_pty_writes();
            let accepted = submitted.take();
            if search_open {
                let search = app.windows[&source_window].tab_states[1].search.as_ref().unwrap();
                assert_eq!(search.query, "needle", "source={source_index} read_only={read_only}");
                assert_eq!(search.cursor(), "needle".len());
                assert_eq!(search.matches.len(), 7);
                assert_eq!(search.current, Some(1));
                assert_eq!(search.requested_scroll_row, None);
            } else {
                assert!(app.windows[&source_window].tab_states[1].search.is_none());
            }
            if read_only || search_open {
                assert_eq!((attempts, accepted), (Vec::new(), Vec::new()));
            } else {
                let expected: Vec<_> = std::iter::once(source)
                    .chain(std::collections::BTreeSet::from([bracketed, peer, inactive]))
                    .map(|pane| {
                        let bytes = if pane == bracketed {
                            b"\x1b[200~le\x1b[201~".to_vec()
                        } else {
                            b"le".to_vec()
                        };
                        (pane, bytes)
                    })
                    .collect();
                assert_eq!(attempts, expected);
                assert_eq!(accepted, expected);
            }
            assert_eq!(
                app.windows[&source_window].tab_states[0].search.as_ref().unwrap().query,
                "inactive"
            );
            for ((id, pane), mode) in windows.iter().zip(&modes) {
                let window = &app.windows[id];
                assert_eq!(&window.copy_mode, mode);
                assert_eq!(window.panes[pane].viewport_top_abs, Some(3));
                assert!(window.notification.is_none());
                if *id != source_window {
                    let search =
                        window.tab_states[window.tabs.active_index()].search.as_ref().unwrap();
                    assert_eq!(search.query, "other");
                    assert_eq!(search.cursor(), "other".len());
                    assert_eq!(search.current, None);
                }
            }
            assert_eq!(app.frontmost_window, Some(other_window));
        }
    }
}

/// A writable search with a missing active pane consumes paste instead of leaking it to its still-armed AllTabs peers.
#[cfg(any(windows, unix))]
#[test]
fn real_pty_search_paste_missing_pane_never_broadcasts() {
    if isolated() {
        return;
    }
    for source_index in 0..3 {
        let (mut app, windows) = search_paste_fixture();
        let (source_window, source) = windows[source_index];
        let (other_window, _) = windows[(source_index + 1) % 3];
        app.broadcast = BroadcastState::On { scope: BroadcastScope::AllTabs, source_pane: source };
        app.frontmost_window = Some(other_window);
        app.__test_set_memory_clipboard("probe");
        let submitted = PtySubmissions::start();
        // Prove that the same source and live peers admit terminal paste before removing only the source pane.
        app.windows.get_mut(&source_window).unwrap().tab_states[0].search = None;
        app.wait_for_input_queues();
        assert!(app.run_action_for_window(&Action::PasteFromClipboard, source_window));
        let peers: std::collections::BTreeSet<_> =
            windows.iter().map(|(_, pane)| *pane).filter(|pane| *pane != source).collect();
        let expected: Vec<_> =
            std::iter::once(source).chain(peers).map(|pane| (pane, b"probe".to_vec())).collect();
        assert_eq!(app.__test_drain_pty_writes(), expected);
        assert_eq!(submitted.take(), expected);
        set_search_paste_query(&mut app, source_window, "keep");
        app.wait_for_input_queues();
        let removed = app.windows.get_mut(&source_window).unwrap().panes.remove(&source).unwrap();
        assert_eq!(app.windows[&source_window].tab_states[0].active_pane, source);
        assert_eq!(app.__test_broadcast_source(), Some(source));
        assert!(app.run_action_for_window(&Action::PasteFromClipboard, source_window));
        let attempts = app.__test_drain_pty_writes();
        let accepted = submitted.take();
        // Restore custody before assertions so InputTestApp owns teardown even when the regression fails.
        app.windows.get_mut(&source_window).unwrap().panes.insert(source, removed);
        for (id, pane) in windows {
            assert_eq!(
                app.windows[&id].tab_states[0].search.as_ref().unwrap().query,
                if id == source_window { "keep" } else { "other" }
            );
            assert_eq!(app.windows[&id].panes[&pane].viewport_top_abs, Some(3));
            assert!(app.windows[&id].copy_mode.is_none());
            assert!(app.windows[&id].notification.is_none());
        }
        assert_eq!(
            (attempts, accepted),
            (Vec::new(), Vec::new()),
            "source={source_index}: an open search owns the paste even without its active pane"
        );
        assert_eq!(app.frontmost_window, Some(other_window));
    }
}

/// READONLY search retains Unicode caret semantics and consumes empty, unavailable, stale-window, and missing-pane pastes locally.
#[cfg(any(windows, unix))]
#[test]
fn real_pty_search_paste_unicode_and_unavailable_targets() {
    use sonicterm_ui::text_edit::TextEdit;
    if isolated() {
        return;
    }
    for source_index in 0..3 {
        let (mut app, windows) = search_paste_fixture();
        let (source_window, source) = windows[source_index];
        let (other_window, _) = windows[(source_index + 1) % 3];
        app.broadcast = BroadcastState::On { scope: BroadcastScope::AllTabs, source_pane: source };
        app.frontmost_window = Some(other_window);
        let submitted = PtySubmissions::start();
        // Successful delivery to these same peers proves that empty captures below are not disconnected-PTY artifacts.
        app.windows.get_mut(&source_window).unwrap().tab_states[0].search = None;
        app.__test_set_memory_clipboard("probe");
        app.wait_for_input_queues();
        assert!(app.run_action_for_window(&Action::PasteFromClipboard, source_window));
        let peers: std::collections::BTreeSet<_> =
            windows.iter().map(|(_, pane)| *pane).filter(|pane| *pane != source).collect();
        let expected: Vec<_> =
            std::iter::once(source).chain(peers).map(|pane| (pane, b"probe".to_vec())).collect();
        assert_eq!(app.__test_drain_pty_writes(), expected);
        assert_eq!(submitted.take(), expected);
        app.wait_for_input_queues();
        app.windows.get_mut(&source_window).unwrap().copy_mode =
            Some(CopyModeState::read_only_at((0, 0)));
        set_search_paste_query(&mut app, source_window, "你🙂好");
        let window = app.windows.get_mut(&source_window).unwrap();
        let parser = window.panes[&source].parser.lock();
        let search = window.tab_states[0].search.as_mut().unwrap();
        search.apply_text_edit(TextEdit::MoveStart, parser.grid());
        search.apply_text_edit(TextEdit::MoveForward, parser.grid());
        drop(parser);
        app.__test_set_memory_clipboard("é\r\n\x1b\x7f");
        assert!(app.run_action_for_window(&Action::PasteFromClipboard, source_window));
        let search = app.windows[&source_window].tab_states[0].search.as_ref().unwrap();
        assert_eq!(
            search.query, "你é🙂好",
            "source={source_index}: READONLY search accepts clipboard text at its caret"
        );
        assert_eq!(search.cursor(), "你é".len());
        assert_eq!(search.matches.len(), 7);
        assert_eq!(search.current, Some(1));
        assert_eq!(search.requested_scroll_row, None);
        assert_eq!((app.__test_drain_pty_writes(), submitted.take()), (Vec::new(), Vec::new()));

        for text in [Some(""), Some("\r\n\t\x1b\x7f"), None] {
            // None removes both sources of clipboard data rather than falling back to the user's native clipboard.
            app.test_clipboard_text = text.map(str::to_owned);
            app.clipboard = None;
            assert!(app.run_action_for_window(&Action::PasteFromClipboard, source_window));
            let search = app.windows[&source_window].tab_states[0].search.as_ref().unwrap();
            assert_eq!(search.query, "你é🙂好");
            assert_eq!(search.cursor(), "你é".len());
            assert_eq!(search.matches.len(), 7);
            assert_eq!(search.current, Some(1));
            assert_eq!(search.requested_scroll_row, None);
            assert_eq!((app.__test_drain_pty_writes(), submitted.take()), (Vec::new(), Vec::new()));
        }
        app.__test_set_memory_clipboard("discard");
        for read_only in [true, false] {
            app.windows.get_mut(&source_window).unwrap().copy_mode =
                read_only.then(|| CopyModeState::read_only_at((0, 0)));
            let removed =
                app.windows.get_mut(&source_window).unwrap().panes.remove(&source).unwrap();
            // Leave both active_pane and the armed AllTabs source stale; no live-source broadcast check may hide a leak.
            assert_eq!(app.windows[&source_window].tab_states[0].active_pane, source);
            assert_eq!(app.__test_broadcast_source(), Some(source));
            assert!(app.run_action_for_window(&Action::PasteFromClipboard, source_window));
            let captures = (app.__test_drain_pty_writes(), submitted.take());
            app.windows.get_mut(&source_window).unwrap().panes.insert(source, removed);
            assert_eq!(
                captures,
                (Vec::new(), Vec::new()),
                "source={source_index} missing-pane read_only={read_only}"
            );
            let search = app.windows[&source_window].tab_states[0].search.as_ref().unwrap();
            assert_eq!(search.query, "你é🙂好");
            assert_eq!(search.cursor(), "你é".len());
            assert_eq!(search.matches.len(), 7);
            assert_eq!(search.current, Some(1));
            for (id, _) in windows {
                assert_eq!(
                    app.windows[&id].tab_states[0].search.as_ref().unwrap().query,
                    if id == source_window { "你é🙂好" } else { "other" }
                );
                assert!(app.windows[&id].notification.is_none());
            }
            assert_eq!(
                app.windows[&source_window]
                    .copy_mode
                    .as_ref()
                    .is_some_and(CopyModeState::is_read_only),
                read_only
            );
        }
        // An explicit removed window is refused rather than using cached focus or main's open search.
        let closed = app.__test_seed_child_window(&["closed"]);
        app.windows.remove(&closed);
        assert!(!app.run_action_for_window(&Action::PasteFromClipboard, closed));
        assert_eq!((app.__test_drain_pty_writes(), submitted.take()), (Vec::new(), Vec::new()));
        for (id, pane) in windows {
            assert_eq!(
                app.windows[&id].tab_states[0].search.as_ref().unwrap().query,
                if id == source_window { "你é🙂好" } else { "other" }
            );
            assert_eq!(app.windows[&id].panes[&pane].viewport_top_abs, Some(3));
            assert!(app.windows[&id].notification.is_none());
        }
        assert_eq!(app.frontmost_window, Some(other_window));
    }
}

/// Retained history gives every real-PTY window a stable search counter and a non-live viewport.
#[cfg(any(windows, unix))]
fn search_paste_fixture() -> (InputTestApp, [(WindowId, u64); 3]) {
    let (mut app, windows) = input_test_windows();
    for (id, pane) in windows {
        let window = app.windows.get_mut(&id).unwrap();
        let pane = window.panes.get_mut(&pane).unwrap();
        let mut parser = pane.parser.lock();
        *parser = Parser::new(Grid::new(40, 3));
        for row in 0..21 {
            if row > 0 {
                parser.advance(b"\r\n");
            }
            parser.advance(if row % 3 == 0 { "needle 你é🙂好".as_bytes() } else { b"other" });
        }
        pane.viewport_top_abs = Some(3);
        drop(parser);
        set_search_paste_query(&mut app, id, "other");
    }
    app.__test_enable_pty_write_log();
    (app, windows)
}

/// Install search on the selected tab so tests cannot accidentally assume that tab zero is active.
#[cfg(any(windows, unix))]
fn set_search_paste_query(app: &mut App, id: WindowId, query: &str) {
    let window = app.windows.get_mut(&id).unwrap();
    let tab = &mut window.tab_states[window.tabs.active_index()];
    let mut search = SearchState::new();
    search.set_query(query, window.panes[&tab.active_pane].parser.lock().grid());
    tab.search = Some(search);
}

#[test]
fn keyboard_quit_confirmation_stays_on_its_source_window() {
    // The warning follows the key's owner without retargeting focus or allowing repeats to confirm quit.
    for source_is_child in [false, true] {
        let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
        app.__test_seed_tab("main");
        let main = app.main_window_id.unwrap();
        let child = app.__test_seed_child_window(&["child"]);
        let (source, other) = if source_is_child { (child, main) } else { (main, child) };
        app.frontmost_window = Some(other);
        app.__test_enable_pty_write_log();

        assert!(app.on_quit_chord_pressed(source, false));
        assert_eq!(
            app.windows[&source].notification.as_ref().map(|bubble| bubble.message.as_str()),
            Some(super::super::quit_hold::QUIT_CONFIRM_PROMPT)
        );
        assert!(app.windows[&other].notification.is_none());
        assert_eq!(app.frontmost_window, Some(other));
        assert!(!app.pending_exit);
        assert!(app.on_quit_chord_pressed(source, true));
        assert!(!app.pending_exit);
        assert!(app.on_quit_chord_pressed(source, false));
        assert!(app.pending_exit);
        assert!(app.__test_drain_pty_writes().is_empty());
    }
}

#[test]
fn stale_keyboard_quit_cannot_arm_a_live_window() {
    // A removed source must not contribute the first press or place a warning on the frontmost window.
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    let main = app.main_window_id.unwrap();
    let child = app.__test_seed_child_window(&["removed"]);
    app.windows.remove(&child);
    app.frontmost_window = Some(main);

    assert!(!app.on_quit_chord_pressed(child, false));
    assert!(app.windows[&main].notification.is_none());
    assert!(!app.pending_exit);
    assert!(app.on_quit_chord_pressed(main, false));
    assert!(!app.pending_exit);
    assert!(app.windows[&main].notification.is_some());
}
