//! Palette and search field clipboard fixtures and routing contracts.

use super::*;

/// Put a real terminal selection under a field so a wrong copy route is observable.
#[cfg(any(windows, unix))]
pub(super) fn seed_terminal_selection(app: &mut App, window: WindowId, pane: u64) {
    let parser = app.windows[&window].panes[&pane].parser.clone();
    let selection = {
        let mut parser = parser.lock();
        parser.advance(b"term");
        let grid = parser.grid();
        let mut selection = Selection::new(0, 0);
        selection.extend(0, 3);
        selection
            .with_content_state(pane, grid.content_seq(), grid.is_alt(), grid.scrollback_evicted())
            .with_content_fingerprint(grid)
    };
    app.windows.get_mut(&window).unwrap().selection = Some(selection);
}

/// Open the requested editable palette mode through the owner's public action route.
#[cfg(any(windows, unix))]
pub(super) fn open_field_palette(
    app: &mut App,
    owner: WindowId,
    mode: sonicterm_ui::command_palette::CommandPaletteMode,
) {
    close_field_palette(app);
    assert!(app.run_action_for_window(&Action::OpenCommandPalette, owner));
    if mode == sonicterm_ui::command_palette::CommandPaletteMode::RenameTab {
        assert!(app.run_action_for_window(&Action::RenameTab, owner));
    }
    assert!(app.command_palette_owns_input(owner));
    assert_eq!(app.command_palette.mode(), mode);
}

/// Reset palette ownership so each case opens its own field.
#[cfg(any(windows, unix))]
pub(super) fn close_field_palette(app: &mut App) {
    app.command_palette.close();
    app.palette_attached_window = None;
    app.tab_edit_target = None;
    app.window_rename_target = None;
    for window in app.windows.values_mut() {
        window.copy_mode = None;
    }
}

/// Route a clipboard action through the explicit source or the real menu bridge.
#[cfg(any(windows, unix))]
pub(super) fn dispatch_field_test_action(
    app: &mut App,
    action: &Action,
    owner: WindowId,
    menu: bool,
) {
    if menu {
        assert!(crate::menubar_bridge::drain().is_empty());
        // Without a native proxy, push_action still queues the action before returning false.
        let _ = crate::menubar_bridge::push_action(action.clone());
        app.__test_drain_menubar_actions();
        assert!(crate::menubar_bridge::drain().is_empty());
    } else {
        assert!(app.run_action_for_window(action, owner));
    }
}

/// Retained history gives every real-PTY window a stable search counter and a non-live viewport.
#[cfg(any(windows, unix))]
pub(super) fn search_paste_fixture() -> (InputTestApp, [(WindowId, u64); 3]) {
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
pub(super) fn set_search_paste_query(app: &mut App, id: WindowId, query: &str) {
    let window = app.windows.get_mut(&id).unwrap();
    let tab = &mut window.tab_states[window.tabs.active_index()];
    let mut search = SearchState::new();
    search.set_query(query, window.panes[&tab.active_pane].parser.lock().grid());
    tab.search = Some(search);
}

#[cfg(test)]
#[path = "test_hooks_keymap_field_fixture_tests.rs"]
mod test_hooks_keymap_field_fixture_tests;
