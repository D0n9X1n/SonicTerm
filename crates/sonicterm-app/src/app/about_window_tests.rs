use sonicterm_cfg::keymap::{Action, BroadcastScope, Keymap};
use sonicterm_cfg::{config::Config, theme::Theme};

use super::*;

/// A headless app with one main tab named `zsh` and an open About window.
fn app_with_about() -> (App, u64, WindowId) {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.test_clipboard_text = Some(String::new());
    let main_pane = app.__test_seed_tab("zsh");
    let about_id = app.__test_open_about_window();
    (app, main_pane, about_id)
}

/// The About pane's id.
fn about_pane(app: &App, about_id: WindowId) -> u64 {
    let window = &app.windows[&about_id];
    window.tab_states[window.tabs.active_index()].active_pane
}

/// Contract: opening About while it is open focuses and refreshes that one window instead of a
/// second, and with none open it only requests one for the next event-loop turn.
#[test]
fn opening_about_twice_reuses_one_window() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("zsh");
    app.show_about();
    assert!(app.about.open_requested, "the first open waits for the event loop");
    assert!(app.about_window_id().is_none());
    let about_id = app.__test_open_about_window();
    assert!(!app.about.open_requested);
    app.show_about();
    assert!(!app.about.open_requested, "an open window is focused, not requested again");
    assert_eq!(app.about_window_id(), Some(about_id));
    assert_eq!(app.frontmost_window, Some(about_id));
    let windows = app.windows.values().filter(|window| window.role == WindowRole::About);
    assert_eq!(windows.count(), 1);
}

/// Contract: the pane shows the rendered rows, Copy puts exactly the report rows on the clipboard
/// and says so, and a refused clipboard is reported as a warning instead of a false success.
#[test]
fn copy_reports_the_real_clipboard_result() {
    let (mut app, _, about_id) = app_with_about();
    let view = app.about.view.clone().expect("the window was filled");
    assert!(view.text.starts_with(&format!("SonicTerm {}", env!("CARGO_PKG_VERSION"))));
    assert!(view.text.contains("[ Copy ]") && !view.copy_text.contains("[ Copy ]"));
    assert!(app.copy_about());
    assert_eq!(app.test_clipboard_text.as_deref(), Some(view.copy_text.as_str()));
    let bubble = app.windows[&about_id].notification.clone().expect("copied notification");
    assert_eq!(bubble.level, sonicterm_ui::overlays::NotificationLevel::Info);
    assert_eq!(bubble.message, "Copied About details");
    app.test_clipboard_write_failure = true;
    assert!(!app.copy_about());
    let bubble = app.windows[&about_id].notification.clone().expect("failure notification");
    assert_eq!(bubble.level, sonicterm_ui::overlays::NotificationLevel::Warning);
    assert_eq!(bubble.message, "Could not copy About details");
}

/// Contract: the copy chord copies, and a press on the drawn Copy button hits it while a press
/// beside it or on the row above does not.
#[test]
fn the_copy_key_and_button_copy() {
    let (mut app, _, about_id) = app_with_about();
    let chord = super::super::memory_report::COPY_ALL_CHORD.to_owned();
    app.handle_about_key(about_id, &[chord], false);
    let copy_text = app.about.view.as_ref().map(|view| view.copy_text.clone());
    assert_eq!(app.test_clipboard_text, copy_text);
    let (row, col) = drawn_button(&app, about_id, 40).expect("the button is drawn");
    app.test_clipboard_text = Some(String::new());
    assert!(!app.about_copy_at(row, col + 8), "past the button");
    assert!(!app.about_copy_at(row - 1, col), "the row above");
    assert_eq!(app.test_clipboard_text.as_deref(), Some(""));
    assert!(app.about_copy_at(row, col + 7), "the button's last cell");
    assert_eq!(app.test_clipboard_text, copy_text);
}

/// Contract: Esc and Cmd+W (close pane or tab) close the About window, never quit, and leave the
/// terminal window untouched.
#[test]
fn escape_and_close_close_about_without_quitting() {
    let (mut app, _, about_id) = app_with_about();
    app.handle_about_key(about_id, &["escape".to_owned()], false);
    assert!(app.about_window_id().is_none());
    assert!(app.about.view.is_none(), "no stale text survives the window");
    let about_id = app.__test_open_about_window();
    assert!(app.run_action_for_window(&Action::CloseActivePaneOrTab, about_id));
    assert!(app.about_window_id().is_none());
    assert!(!app.should_exit() && !app.pending_exit);
    assert_eq!(app.__test_main_tab_count(), 1);
}

/// Contract: About is not a terminal window: closing the last terminal tab still exits with it
/// open, it is never a tab drop target, and a broadcast never reaches its pane.
#[test]
fn about_is_excluded_from_terminal_counts_drop_targets_and_broadcast() {
    let (mut app, main_pane, about_id) = app_with_about();
    assert_eq!(app.child_window_count(), 0, "About is not counted as a terminal window");
    assert!(WindowRole::About.is_report(), "drop targets and tab drags skip report windows");
    assert!(!WindowRole::Terminal.is_report());
    let second = app.__test_seed_tab("second");
    app.frontmost_window = app.main_window_id;
    app.__test_set_active_pane(0, main_pane);
    app.toggle_broadcast(BroadcastScope::AllTabs);
    let receivers = app.__test_broadcast_receivers();
    assert!(receivers.iter().all(|pane| *pane == main_pane || *pane == second), "{receivers:?}");
    assert!(!receivers.contains(&about_pane(&app, about_id)));
    assert!(app.run_action(&Action::CloseTab));
    assert!(app.run_action(&Action::CloseTab));
    assert!(app.windows.contains_key(&about_id));
    assert!(app.pending_exit, "the last terminal close requests exit despite About");
}

/// Contract: the About pane has no PTY, so a typed key or paste aimed at it reaches no shell.
#[test]
fn keys_and_pastes_in_about_never_reach_a_pty() {
    let (mut app, _, about_id) = app_with_about();
    assert!(app.windows[&about_id].panes.values().all(|pane| pane.pty.is_none()));
    app.__test_enable_pty_write_log();
    app.test_clipboard_text = Some("rm -rf /\n".to_string());
    assert!(app.run_action_for_window(&Action::PasteFromClipboard, about_id));
    app.handle_about_key(about_id, &["x".to_string()], false);
    assert!(app.__test_pty_write_log().is_empty());
}

/// Contract: the Config row reports the last load error until a load succeeds, and a missing file
/// as defaults in use.
#[test]
fn the_config_row_follows_the_last_load_result() {
    let (mut app, _, _) = app_with_about();
    app.runtime_config_path = Some(std::path::PathBuf::from("/nonexistent/sonicterm.toml"));
    assert_eq!(app.collect_about_facts().config_status, ConfigStatus::Missing);
    let error = anyhow::anyhow!("TOML parse error at line 3, column 1");
    app.set_config_reload_error(Some(&error));
    assert_eq!(
        app.collect_about_facts().config_status,
        ConfigStatus::Failed("TOML parse error at line 3, column 1".into())
    );
    app.set_config_reload_error(None);
    assert_eq!(app.collect_about_facts().config_status, ConfigStatus::Missing);
}

/// Contract: the About action opens About, so every route that dispatches it (keymap, palette and
/// both menus) reaches the same window.
#[test]
fn the_about_action_opens_about() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("zsh");
    assert!(app.run_action(&Action::ShowAbout));
    assert!(app.about.open_requested);
}

/// Contract: a reload that names a theme and a keymap that do not exist keeps the active ones,
/// and About shows those active names, each with its reload error, never the names that failed.
#[test]
fn theme_and_keymap_rows_show_the_active_ones_and_their_reload_errors() {
    let dir = std::env::temp_dir().join(format!("sonicterm-about-reload-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sonicterm.toml");
    std::fs::write(&path, "").unwrap();
    let (mut app, _, _) = app_with_about();
    app.runtime_config_path = Some(path.clone());
    let (theme, keymap) = (app.theme.name.clone(), app.keymap.meta.name.clone());
    assert!(!theme.is_empty() && !keymap.is_empty());
    std::fs::write(&path, "theme = \"no-such-theme\"\nkeymap = \"no-such-keymap\"\n").unwrap();
    app.force_reload_config();
    assert_eq!((app.theme.name.as_str(), app.keymap.meta.name.as_str()), (&*theme, &*keymap));
    app.show_about();
    let text = app.about.view.as_ref().expect("About is filled").text.clone();
    let row = |label: &str| {
        text.lines().find(|line| line.starts_with(label)).unwrap_or_default().to_owned()
    };
    let theme_row = row("Theme");
    assert!(theme_row.starts_with(&format!("Theme      {theme}  ✗ ")), "{text}");
    let keymap_row = row("Keymap");
    assert!(keymap_row.starts_with(&format!("Keymap     {keymap}  ✗ ")), "{text}");
    let _ = std::fs::remove_dir_all(dir);
}

/// The text of view row `row` of the About pane, as it is drawn now.
fn drawn_row(app: &App, about_id: WindowId, row: u16) -> String {
    let pane = &app.windows[&about_id].panes[&about_pane(app, about_id)];
    let parser = pane.parser.lock();
    let grid = parser.grid();
    let abs = pane.resolved_view_top(grid) + u64::from(row);
    let last = (usize::from(grid.cols) - 1, abs);
    sonicterm_ui::selection::plain_text_from_grid_range(grid, (0, abs), last)
}

/// Where the Copy button is drawn in the view: its row and first column.
fn drawn_button(app: &App, about_id: WindowId, rows: u16) -> Option<(u16, u16)> {
    (0..rows).find_map(|row| {
        let text = drawn_row(app, about_id, row);
        (text.trim() == "[ Copy ]").then(|| {
            let indent = text.len() - text.trim_start().len();
            (row, u16::try_from(indent).unwrap())
        })
    })
}

/// Contract: Copy is hit where it is drawn even when a row is wider than the pane. A long config
/// error wraps at 80 columns, the button stays whole on its own row, and a click on it copies.
#[test]
fn a_click_on_the_drawn_button_copies_when_rows_wrap() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.test_clipboard_text = Some(String::new());
    app.__test_seed_tab("zsh");
    let error = anyhow::anyhow!("TOML parse error at line 3: {}", "x".repeat(150));
    app.set_config_reload_error(Some(&error));
    let about_id = app.__test_open_about_window_sized(80, 24);
    let view = app.about.view.clone().expect("About is filled");
    assert!(view.width > 80, "a row is wider than the pane");
    let (row, col) = drawn_button(&app, about_id, 24).expect("the button is drawn whole");
    assert!(!app.about_copy_at(row, col.saturating_sub(1)), "beside the button is text");
    assert!(app.about_copy_at(row, col), "a click on the drawn button copies");
    assert_eq!(app.test_clipboard_text.as_deref(), Some(view.copy_text.as_str()));
}

/// Contract: Copy follows the scrolled view. Scrolled to the bottom, the button is drawn on another
/// view row and a click there copies; scrolled back to the top, that cell shows a report row.
#[test]
fn a_click_on_the_drawn_button_copies_in_a_scrolled_view() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.test_clipboard_text = Some(String::new());
    app.__test_seed_tab("zsh");
    let about_id = app.__test_open_about_window_sized(100, 6);
    assert_eq!(drawn_button(&app, about_id, 6), None, "the view starts at the top");
    let pane_id = about_pane(&app, about_id);
    let scrollbar = app.config.appearance.scrollbar;
    let window = app.windows.get_mut(&about_id).unwrap();
    super::super::child_window::scroll_child_pane(window, pane_id, i32::MAX, scrollbar);
    let (row, col) = drawn_button(&app, about_id, 6).expect("the bottom view shows the button");
    assert!(app.about_copy_at(row, col), "a click on the drawn button copies");
    let copied = app.about.view.as_ref().map(|view| view.copy_text.clone());
    assert_eq!(app.test_clipboard_text, copied);
    app.test_clipboard_text = Some(String::new());
    let window = app.windows.get_mut(&about_id).unwrap();
    super::super::child_window::scroll_child_pane(window, pane_id, i32::MIN, scrollbar);
    assert!(!app.about_copy_at(row, col), "at the top that cell is a report row");
    assert_eq!(app.test_clipboard_text.as_deref(), Some(""));
}

/// Connect an assistive technology to About window `about_id` and flush, as the event loop would.
fn connect_reader(app: &mut App, about_id: WindowId) {
    use crate::app::accessibility::{AccessibilityRequest, WindowAccessibility};
    app.accessibility.insert(about_id, WindowAccessibility::default());
    app.handle_accessibility_request(about_id, AccessibilityRequest::InitialTree);
    let _ = app.flush_accessibility(std::time::Instant::now());
}

/// The labels of the last About tree's window children, in order, with each node's role.
fn read_tree(app: &App, about_id: WindowId) -> Vec<(accesskit::Role, String, bool)> {
    use crate::app::accessibility::ROOT_NODE;
    let tree = app.accessibility[&about_id].sent.last().cloned().expect("a tree was sent");
    let node = |id: accesskit::NodeId| {
        tree.nodes.iter().find(|(node_id, _)| *node_id == id).map(|(_, node)| node.clone())
    };
    let root = node(ROOT_NODE).expect("the window node");
    root.children()
        .iter()
        .filter_map(|id| node(*id))
        .map(|child| {
            let clicks = child.supports_action(accesskit::Action::Click);
            (child.role(), child.label().unwrap_or_default().to_owned(), clicks)
        })
        .collect()
}

/// Contract: a screen reader reads every About row, in order, as one node with its label and
/// value, then the Copy button as a button it can press.
#[test]
fn the_about_tree_reads_every_row_in_order_then_copy() {
    let (mut app, _, about_id) = app_with_about();
    connect_reader(&mut app, about_id);
    let nodes = read_tree(&app, about_id);
    let labels: Vec<&str> = nodes.iter().map(|(_, label, _)| label.as_str()).collect();
    let prefixes = [
        "Version: SonicTerm ",
        "System: ",
        "Renderer: ",
        "Font: ",
        "Theme: ",
        "Keymap: ",
        "Config: ",
        "Logs: ",
    ];
    assert_eq!(labels.len(), prefixes.len() + 1, "{labels:?}");
    for (label, prefix) in labels.iter().zip(prefixes) {
        assert!(label.starts_with(prefix), "{label:?} should start with {prefix:?}: {labels:?}");
    }
    assert!(nodes[..prefixes.len()]
        .iter()
        .all(|(role, _, clicks)| *role == accesskit::Role::Label && !clicks));
    assert_eq!(nodes.last().unwrap(), &(accesskit::Role::Button, "Copy".to_owned(), true));
}

/// Contract: pressing the Copy node copies the rows, exactly as the button does.
#[test]
fn the_about_copy_node_copies() {
    use crate::app::accessibility::{AccessibilityRequest, ABOUT_COPY_NODE};
    let (mut app, _, about_id) = app_with_about();
    connect_reader(&mut app, about_id);
    let request = AccessibilityRequest::Click { node: ABOUT_COPY_NODE.0, target: None };
    app.handle_accessibility_request(about_id, request);
    let copied = app.about.view.as_ref().map(|view| view.copy_text.clone());
    assert_eq!(app.test_clipboard_text, copied);
}

/// Contract: reading the facts again with R updates what a screen reader reads.
#[test]
fn a_refresh_updates_the_about_tree() {
    let (mut app, _, about_id) = app_with_about();
    connect_reader(&mut app, about_id);
    let config = |app: &App| {
        read_tree(app, about_id)
            .into_iter()
            .find(|(_, label, _)| label.starts_with("Config: "))
            .map(|(_, label, _)| label)
            .unwrap_or_default()
    };
    assert!(!config(&app).contains('✗'), "{}", config(&app));
    let error = anyhow::anyhow!("TOML parse error at line 3, column 1");
    app.set_config_reload_error(Some(&error));
    app.handle_about_key(about_id, &["r".to_owned()], false);
    let _ = app.flush_accessibility(std::time::Instant::now());
    assert!(config(&app).contains("✗ TOML parse error at line 3, column 1"), "{}", config(&app));
}

/// Resize the About pane's grid to `cols` by `rows`, as a font, reload or window change does.
fn resize_about_pane(app: &App, about_id: WindowId, cols: u16, rows: u16) {
    let pane = &app.windows[&about_id].panes[&about_pane(app, about_id)];
    pane.parser.lock().grid_mut().resize(cols, rows);
}

/// Whether the drawn Copy button sits whole at the pane's right edge and a click on it copies.
fn assert_button_whole_at_edge(app: &mut App, about_id: WindowId, cols: u16, case: &str) {
    let (row, col) = drawn_button(app, about_id, 24).unwrap_or_else(|| panic!("{case}: not drawn"));
    assert_eq!(col + 8, cols, "{case}: right-aligned inside {cols} columns");
    app.test_clipboard_text = Some(String::new());
    assert!(app.about_copy_at(row, col), "{case}: a click on the button copies");
    let copied = app.about.view.as_ref().map(|view| view.copy_text.clone());
    assert_eq!(app.test_clipboard_text, copied, "{case}");
}

/// Contract: a font zoom in either direction resizes the pane without a window resize, and About
/// is laid out again for the new width, so Copy stays whole at the edge and a click on it copies.
#[test]
fn copy_stays_whole_after_font_zoom() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("zsh");
    let error = anyhow::anyhow!("TOML parse error at line 3: {}", "x".repeat(150));
    app.set_config_reload_error(Some(&error));
    let about_id = app.__test_open_about_window_sized(80, 24);
    assert_button_whole_at_edge(&mut app, about_id, 80, "opened");
    assert!(app.run_action(&Action::IncreaseFontSize));
    resize_about_pane(&app, about_id, 74, 24);
    app.relayout_about_if_resized();
    assert_button_whole_at_edge(&mut app, about_id, 74, "zoomed in");
    assert!(app.run_action(&Action::DecreaseFontSize));
    assert!(app.run_action(&Action::DecreaseFontSize));
    resize_about_pane(&app, about_id, 90, 24);
    app.relayout_about_if_resized();
    assert_button_whole_at_edge(&mut app, about_id, 90, "zoomed out");
}

/// Contract: a reload that changes the font size resizes the pane, and About is laid out again
/// for it, so Copy stays whole at the edge and a click on it copies.
#[test]
fn copy_stays_whole_after_a_reload_changes_the_font_size() {
    let dir = std::env::temp_dir().join(format!("sonicterm-about-font-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("sonicterm.toml");
    std::fs::write(&path, "").unwrap();
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("zsh");
    app.runtime_config_path = Some(path.clone());
    let error = anyhow::anyhow!("TOML parse error at line 3: {}", "x".repeat(150));
    app.set_config_reload_error(Some(&error));
    let about_id = app.__test_open_about_window_sized(80, 24);
    std::fs::write(&path, "[font]\nsize = 16\n").unwrap();
    app.force_reload_config();
    assert_eq!(app.config.font.size, 16.0, "the reload applied the new size");
    app.set_config_reload_error(Some(&error));
    resize_about_pane(&app, about_id, 70, 24);
    app.relayout_about_if_resized();
    assert_button_whole_at_edge(&mut app, about_id, 70, "reloaded");
    let _ = std::fs::remove_dir_all(dir);
}

/// About at 80x24 with a long config error, so Copy sits at the right edge below the rows.
fn app_with_wide_about() -> (App, WindowId) {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("zsh");
    let error = anyhow::anyhow!("TOML parse error at line 3: {}", "x".repeat(150));
    app.set_config_reload_error(Some(&error));
    let about_id = app.__test_open_about_window_sized(80, 24);
    (app, about_id)
}

/// Contract: a height-only resize lays About out again. Shrinking the pane to 10 rows drops the
/// bottom rows, and restoring it to 24 at the same width brings Copy back whole and clickable.
#[test]
fn copy_returns_after_a_height_shrink_and_restore() {
    let (mut app, about_id) = app_with_wide_about();
    assert_button_whole_at_edge(&mut app, about_id, 80, "opened");
    resize_about_pane(&app, about_id, 80, 10);
    app.relayout_about_if_resized();
    resize_about_pane(&app, about_id, 80, 24);
    app.relayout_about_if_resized();
    assert_button_whole_at_edge(&mut app, about_id, 80, "height restored");
}

/// Contract: a resize that changes the width and the height together lays About out once for the
/// new size, and back again, with Copy whole at the edge each time.
#[test]
fn copy_stays_whole_when_width_and_height_change_together() {
    let (mut app, about_id) = app_with_wide_about();
    resize_about_pane(&app, about_id, 70, 12);
    app.relayout_about_if_resized();
    resize_about_pane(&app, about_id, 90, 24);
    app.relayout_about_if_resized();
    assert_button_whole_at_edge(&mut app, about_id, 90, "resized both ways");
}

/// Contract: after a height-only resize, a screen reader still reads every row in order, then Copy.
#[test]
fn the_about_tree_keeps_every_row_after_a_height_change() {
    let (mut app, about_id) = app_with_wide_about();
    connect_reader(&mut app, about_id);
    let before = read_tree(&app, about_id);
    resize_about_pane(&app, about_id, 80, 10);
    app.relayout_about_if_resized();
    let _ = app.flush_accessibility(std::time::Instant::now());
    let after = read_tree(&app, about_id);
    assert_eq!(after.len(), 9, "eight rows and Copy: {after:?}");
    assert_eq!(after, before);
}
