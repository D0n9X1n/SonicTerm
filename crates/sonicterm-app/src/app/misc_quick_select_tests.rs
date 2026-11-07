//! Pins quick select's viewport contract: it labels only the pane's displayed rows, at their
//! scrollback-absolute positions, whether the view is scrolled back or live, and its row counter
//! records only the rows the scan visited.

use super::*;

/// Quick select labels the pane's displayed viewport: scrolled back, it hints only the history URL
/// on screen, at its scrollback-absolute row, starts its cursor at the view's top, and scans one row
/// per displayed row; on the live view it hints only the live screen's URL.
#[test]
fn quick_select_labels_the_displayed_viewport() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.force_frame_counters_on().expect("no window exists yet");
    let window = app.__test_seed_child_window(&["quick select"]);
    let pane = app.__test_child_pane_ids(window).unwrap()[0];
    app.registry.windows[&window].panes[&pane].parser.lock().grid_mut().resize(30, 2);
    let output = b"old http://h.co\r\nplain\r\nmid https://m.io\r\nlive http://l.co\r\n";
    assert!(app.__test_advance_child_pane_parser(window, pane, output));
    let history_rows =
        app.registry.windows[&window].panes[&pane].parser.lock().grid().scrollback_len();
    assert!(history_rows >= 3, "the fixture scrolls rows into history");

    app.registry
        .windows
        .get_mut(&window)
        .unwrap()
        .panes
        .get_mut(&pane)
        .unwrap()
        .pin_viewport_top(Some(1));
    app.enter_quick_select_for_kind(FrontmostKind::Child(window));
    let state = app.registry.windows[&window].copy_mode.clone().expect("quick select entered");
    let hints = state.quick_select.expect("hints").hints;
    let seen: Vec<(usize, usize, &str)> =
        hints.iter().map(|hint| (hint.row, hint.col_start, hint.text.as_str())).collect();
    assert_eq!(seen, vec![(2, 4, "https://m.io")]);
    assert_eq!(state.cursor, (0, 1), "the cursor starts at the displayed top");
    let rows_scanned = app.frame_counters.as_ref().unwrap().hover.quick_select_rows_scanned;
    assert_eq!(rows_scanned, 2, "one scanned row per displayed row");

    app.registry
        .windows
        .get_mut(&window)
        .unwrap()
        .panes
        .get_mut(&pane)
        .unwrap()
        .pin_viewport_top(None);
    app.enter_quick_select_for_kind(FrontmostKind::Child(window));
    let hints =
        app.registry.windows[&window].copy_mode.clone().unwrap().quick_select.unwrap().hints;
    assert!(hints.iter().all(|hint| hint.row >= history_rows), "{hints:?}");
    assert!(!hints.iter().any(|hint| hint.text == "https://m.io"));
}

/// The quick-select rows counter records the rows the scan read: a viewport whose first 27 rows
/// hold URLs ends the scan at the 27th, so the 13 rows below it are never visited or counted.
#[test]
fn quick_select_counts_only_the_rows_it_visited() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.force_frame_counters_on().expect("no window exists yet");
    let window = app.__test_seed_child_window(&["quick select rows"]);
    let pane = app.__test_child_pane_ids(window).unwrap()[0];
    app.registry.windows[&window].panes[&pane].parser.lock().grid_mut().resize(20, 40);
    let output: String = (1..=30).map(|row| format!("\x1b[{row};1Hhttp://a{row}.com")).collect();
    assert!(app.__test_advance_child_pane_parser(window, pane, output.as_bytes()));
    app.enter_quick_select_for_kind(FrontmostKind::Child(window));
    let hints =
        app.registry.windows[&window].copy_mode.clone().unwrap().quick_select.unwrap().hints;
    assert_eq!(hints.len(), 26);
    let rows_scanned = app.frame_counters.as_ref().unwrap().hover.quick_select_rows_scanned;
    assert_eq!(rows_scanned, 27, "the scan stops at the 27th URL's row of the 40 displayed");
}
