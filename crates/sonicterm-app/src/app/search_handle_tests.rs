use super::*;
use sonicterm_ui::search::{SearchMode, SearchState};

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

/// Search paste is window-local single-line input, never PTY or broadcast traffic.
#[test]
fn search_paste_updates_main_and_child_counter_without_pty_delivery() {
    let _serialised = crate::app::media::MEDIA_COUNTER_LOCK.lock();
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
