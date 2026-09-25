use super::*;
use sonicterm_ui::search::{SearchMode, SearchState};

#[cfg(target_os = "macos")]
#[test]
fn native_mac_search_deletion_uses_the_source_window_and_preserves_viewport() {
    // Production search editing owns modified Backspace independently of focus and never forwards it to a terminal.
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    let main = app.main_window_id.unwrap();
    let child = app.__test_seed_child_window(&["child"]);
    app.__test_enable_pty_write_log();
    let key = Key::Named(NamedKey::Backspace);
    for (owner, other) in [(main, child), (child, main)] {
        for (mods, before, after) in [
            (ModifiersState::SUPER, "alpha beta", ""),
            (ModifiersState::ALT, "alpha beta", "alpha "),
            (ModifiersState::CONTROL, "alpha é", "alpha e"),
        ] {
            let window = app.windows.get_mut(&owner).unwrap();
            let pane_id = window.tab_states[0].active_pane;
            let pane = window.panes.get_mut(&pane_id).unwrap();
            *pane.parser.lock() = history_parser();
            pane.viewport_top_abs = Some(3);
            let mut search = SearchState::new();
            search.set_query(before, pane.parser.lock().grid());
            window.tab_states[0].search = Some(search);
            app.frontmost_window = Some(other);
            let edit = super::super::text_edit::search_text_edit_for_key(&key, mods);
            assert!(app.search_handle_key_parts(owner, &key, mods, edit, None));
            assert_eq!(app.windows[&owner].tab_states[0].search.as_ref().unwrap().query, after);
            assert_eq!(app.windows[&owner].panes[&pane_id].viewport_top_abs, Some(3));
            assert_eq!(app.frontmost_window, Some(other));
            assert!(app.__test_drain_pty_writes().is_empty());
        }
    }
}

#[test]
fn unsupported_modified_backspace_keeps_search_text_and_owner() {
    // An unsupported chord is not plain Backspace, and its live search remains installed after rejection.
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    let owner = app.main_window_id.unwrap();
    let pane = app.windows[&owner].tab_states[0].active_pane;
    let mut search = SearchState::new();
    search.set_query("keep", app.windows[&owner].panes[&pane].parser.lock().grid());
    app.windows.get_mut(&owner).unwrap().tab_states[0].search = Some(search);
    let key = Key::Named(NamedKey::Backspace);
    let mods = ModifiersState::CONTROL | ModifiersState::ALT;
    let edit = super::super::text_edit::search_text_edit_for_key(&key, mods);
    assert_eq!(edit, None);
    assert!(!app.search_handle_key_parts(owner, &key, mods, edit, None));
    assert_eq!(app.windows[&owner].tab_states[0].search.as_ref().unwrap().query, "keep");
    // Shift alone is still plain Backspace, not an unsupported command chord.
    assert!(app.search_handle_key_parts(owner, &key, ModifiersState::SHIFT, None, None));
    assert_eq!(app.windows[&owner].tab_states[0].search.as_ref().unwrap().query, "kee");
}

fn history_parser() -> Parser {
    let mut parser = Parser::new(Grid::new(40, 3));
    for row in 0..21 {
        if row > 0 {
            parser.advance(b"\r\n");
        }
        parser.advance(if row % 3 == 0 { b"needle" } else { b"other" });
    }
    parser
}

/// Navigation uses global order, reveals offscreen selections first, and consumes scrolling exactly once.
#[test]
fn viewport_search_reveals_then_advances_and_wraps() {
    let parser = history_parser();
    let grid = parser.grid();
    let mut search = SearchState::new();
    search.set_query("needle", grid);
    search.anchor_to_viewport(12);
    assert_eq!(search.current, Some(4));
    navigate_search(&mut search, 12, grid.rows, true);
    assert_eq!(search.current, Some(3));
    assert_eq!(take_search_scroll(&mut search, grid, 12), Some(Some(8)));
    assert_eq!(take_search_scroll(&mut search, grid, 12), None);
    search.anchor_to_viewport(10);
    assert_eq!(search.current, Some(4));
    navigate_search(&mut search, 10, 2, false);
    assert_eq!(search.current, Some(4));
    assert_eq!(search.requested_scroll_row.take(), Some(12));
    navigate_search(&mut search, 12, grid.rows, false);
    assert_eq!(search.current, Some(5));
    search.anchor_to_viewport(18);
    navigate_search(&mut search, 18, grid.rows, false);
    assert_eq!(search.current, Some(0));
    assert_eq!(take_search_scroll(&mut search, grid, 18), Some(Some(0)));
    navigate_search(&mut search, 0, grid.rows, true);
    assert_eq!(search.current, Some(6));
    assert_eq!(take_search_scroll(&mut search, grid, 0), Some(Some(17)));
}

/// Refinement anchors to the new viewed position; regex errors and empty queries retain no phantom selection.
#[test]
fn viewport_search_query_changes_and_refresh_are_stable() {
    let parser = history_parser();
    let grid = parser.grid();
    let mut search = SearchState::new();
    prepare_search(&mut search, 7, grid, 12);
    search.input_str("need", grid);
    anchor_unfocused_search(&mut search, 12);
    assert_eq!(search.current, Some(4));
    search.next();
    search.requested_scroll_row.take();
    search.input_str("le", grid);
    anchor_unfocused_search(&mut search, 14);
    assert_eq!(search.current, Some(5));
    assert_eq!(search.requested_scroll_row, None);
    prepare_search(&mut search, 7, grid, 0);
    assert_eq!(search.current, Some(5));
    search.mode = SearchMode::Regex;
    search.set_query("[", grid);
    anchor_unfocused_search(&mut search, 12);
    assert!(search.regex_error.is_some());
    assert_eq!(search.current, None);
    search.set_query("", grid);
    anchor_unfocused_search(&mut search, 12);
    assert_eq!(search.current, None);
}

/// Historical eviction rebases a surviving match, and screen switches discard it without a stale scroll request.
#[test]
fn search_refresh_rebases_eviction_and_clears_screen_identity() {
    let mut parser = history_parser();
    let mut search = SearchState::new();
    search.bind_pane(7);
    search.set_query("needle", parser.grid());
    search.anchor_to_viewport(12);
    assert_eq!(search.current_match().unwrap().row, 12);
    parser.grid_mut().set_scrollback_limit(12);
    assert!(search.maybe_refresh_for_revision(parser.grid()));
    assert_eq!(search.current_match().unwrap().row, 6);
    assert_eq!(search.requested_scroll_row, None);
    parser.advance(b"\x1b[?1049h");
    assert!(search.maybe_refresh_for_revision(parser.grid()));
    assert_eq!(search.current, None);
    assert!(search.matches.is_empty());
}

#[test]
fn source_window_search_commit_retains_viewport_and_ignores_frontmost() {
    // Shared commit routing anchors each window's own retained history without typing into its peer.
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    let main = app.main_window_id.unwrap();
    let child = app.__test_seed_child_window(&["child"]);
    for (owner, other, view_top, selected) in [(main, child, 12, 4), (child, main, 3, 1)] {
        let pane_id = app.windows[&owner].tab_states[0].active_pane;
        let window = app.windows.get_mut(&owner).unwrap();
        window.tab_states[0].search = Some(SearchState::new());
        let pane = window.panes.get_mut(&pane_id).unwrap();
        *pane.parser.lock() = history_parser();
        pane.viewport_top_abs = Some(view_top);
        app.frontmost_window = Some(other);
        app.__test_enable_pty_write_log();
        assert!(app.search_handle_ime_commit(owner, "needle"));
        let search = app.windows[&owner].tab_states[0].search.as_ref().unwrap();
        assert_eq!(search.query, "needle");
        assert_eq!(search.current, Some(selected));
        assert_eq!(search.matches.len(), 7);
        assert_eq!(search.requested_scroll_row, None);
        assert_eq!(app.windows[&owner].panes[&pane_id].viewport_top_abs, Some(view_top));
        assert!(app.__test_drain_pty_writes().is_empty());
        assert_eq!(app.frontmost_window, Some(other));
    }
}

#[test]
fn source_window_search_commit_retains_missing_pane_state() {
    // A temporarily absent pane keeps its search query, while a stale window never borrows main's search.
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    let main = app.main_window_id.unwrap();
    let child = app.__test_seed_child_window(&["child"]);
    for owner in [main, child] {
        let pane_id = app.windows[&owner].tab_states[0].active_pane;
        app.windows.get_mut(&owner).unwrap().tab_states[0].search = Some(SearchState::new());
        assert!(app.search_handle_ime_commit(owner, "keep"));
        let pane = app.windows.get_mut(&owner).unwrap().panes.remove(&pane_id).unwrap();
        assert!(!app.search_handle_ime_commit(owner, "discard"));
        assert_eq!(app.windows[&owner].tab_states[0].search.as_ref().unwrap().query, "keep");
        app.windows.get_mut(&owner).unwrap().panes.insert(pane_id, pane);
    }
    assert!(!app.search_handle_ime_commit(WindowId::from(0), "wrong"));
    assert_eq!(app.windows[&main].tab_states[0].search.as_ref().unwrap().query, "keep");
    assert_eq!(app.windows[&child].tab_states[0].search.as_ref().unwrap().query, "keep");
}

/// Search paste is window-local single-line input, never PTY or broadcast traffic.
#[test]
fn search_paste_updates_main_and_child_counter_without_pty_delivery() {
    for child in [false, true] {
        let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
        app.__test_seed_tab("main");
        let window = if child {
            app.__test_seed_child_window(&["child"])
        } else {
            app.main_window_id.unwrap()
        };
        let pane = app.windows[&window].tab_states[0].active_pane;
        *app.pane_by_id(pane).unwrap().parser.lock() = history_parser();
        app.windows.get_mut(&window).unwrap().panes.get_mut(&pane).unwrap().viewport_top_abs =
            Some(12);
        if child {
            app.open_search_in_child(window);
        } else {
            app.open_search();
        }
        app.test_clipboard_text = Some("nee\r\ndle\x1b\x7f".into());
        app.__test_enable_pty_write_log();
        let kind = if child {
            super::super::FrontmostKind::Child(window)
        } else {
            super::super::FrontmostKind::Main
        };
        app.paste_clipboard_for_kind(kind);
        let search = app.windows[&window].tab_states[0].search.as_ref().unwrap();
        assert_eq!(search.query, "needle");
        assert_eq!(search.current, Some(4));
        assert_eq!(search.matches.len(), 7);
        assert!(app.__test_pty_write_log().is_empty());
        assert_eq!(app.windows[&window].panes[&pane].viewport_top_abs, Some(12));
    }
}

/// A multi-character key event reaches search as one edit, so history is rescanned once.
#[test]
fn multi_character_key_event_rescans_search_history_once() {
    let parser = history_parser();
    let grid = parser.grid();
    let mut search = SearchState::new();
    let key = Key::Character("ne".into());
    let (handled, keep_search, _) =
        apply_search_key(&mut search, grid, &key, ModifiersState::empty(), None, Some("ne"), 0);
    assert!(handled && keep_search);
    assert_eq!(search.query, "ne");
    assert_eq!(search.matches.len(), 7);
    assert_eq!(search.work().full_scans, 1);
}
