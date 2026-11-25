use super::*;
use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};
use sonicterm_grid::grid::{CellFlags, Color};
use winit::keyboard::SmolStr;

/// A main window with one seeded pane whose row 0 reads "abc", in copy mode with "abc" selected.
fn copy_mode_app() -> (App, WindowId, u64, CopyModeState) {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let pane_id = app.__test_seed_tab("main");
    let window_id = app.registry.main_window_id.expect("seeded main window");
    let window = app.registry.windows.get_mut(&window_id).unwrap();
    {
        let mut parser = window.panes.get(&pane_id).unwrap().parser.lock();
        let grid = parser.grid_mut();
        grid.goto(0, 0);
        for character in "abc".chars() {
            grid.put_char(character, Color::Default, Color::Default, CellFlags::empty());
        }
    }
    let mut state = CopyModeState::new_at((0, 0));
    state.start_select();
    // Copy-mode positions are (column, row): the cursor ends on row 0's third cell.
    state.cursor = (2, 0);
    window.copy_mode = Some(state.clone());
    (app, window_id, pane_id, state)
}

fn yank(app: &mut App, window_id: WindowId) {
    app.handle_window_copy_key(window_id, &Key::Character(SmolStr::new("y")));
}

/// Contract: a copy-mode copy the clipboard refuses returns copy mode with its selection, as a
/// refused selection copy keeps its selection; a copy that succeeds leaves copy mode.
#[test]
fn refused_copy_mode_copy_keeps_copy_mode_and_its_selection() {
    let (mut app, window_id, _, state) = copy_mode_app();
    app.__test_set_memory_clipboard("before");
    app.testing.test_clipboard_write_failure = true;
    yank(&mut app, window_id);
    let restored = app.registry.windows[&window_id].copy_mode.clone();
    assert_eq!(restored, Some(state), "the refused copy returned copy mode and its selection");
    assert_eq!(app.testing.test_clipboard_text.as_deref(), Some("before"), "nothing was written");

    app.testing.test_clipboard_write_failure = false;
    yank(&mut app, window_id);
    assert!(app.registry.windows[&window_id].copy_mode.is_none(), "a written copy exits");
    assert_eq!(app.testing.test_clipboard_text.as_deref(), Some("abc"));
}

/// Contract: a refused copy's state is not returned once the user has started another copy mode
/// or moved to another pane, whose grid its cursor would not name.
#[test]
fn refused_copy_is_not_returned_over_a_newer_owner() {
    let (mut app, window_id, pane_id, state) = copy_mode_app();
    let newer = CopyModeState::new_at((0, 1));
    app.registry.windows.get_mut(&window_id).unwrap().copy_mode = Some(newer.clone());
    app.restore_copy_mode(window_id, pane_id, Some(state.clone()));
    let kept = app.registry.windows[&window_id].copy_mode.clone();
    assert_eq!(kept, Some(newer), "a copy mode started since then is kept");

    app.registry.windows.get_mut(&window_id).unwrap().copy_mode = None;
    app.restore_copy_mode(window_id, pane_id + 1000, Some(state));
    assert!(app.registry.windows[&window_id].copy_mode.is_none(), "another pane gets nothing");
}
