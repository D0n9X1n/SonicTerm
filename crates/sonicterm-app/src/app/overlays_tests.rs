use super::App;
use sonicterm_cfg::config::Config;
use sonicterm_cfg::keymap::{Action, Keymap};
use sonicterm_cfg::theme::Theme;
use sonicterm_ui::command_palette::PaletteEntry;
use winit::keyboard::{Key, NamedKey};

#[test]
fn reopening_tab_selector_in_another_window_selects_its_first_tab() {
    // A fresh selector must discard the previous window's selected TabId, in both directions.
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    let main = app.main_window_id.unwrap();
    let child = app.__test_seed_child_window(&["child", "peer"]);
    for window in [main, child, main, child] {
        app.open_tab_selector(window);
        let expected = app.windows[&window].tabs.tabs()[0].id;
        assert!(
            matches!(app.command_palette.current(), Some(PaletteEntry::Tab { id, .. }) if *id == expected)
        );
        app.command_palette.close();
    }
}

#[test]
fn palette_input_is_owned_by_its_attached_window_only() {
    // Focus changes must not let main, child, or sibling IME edits mutate another window's palette.
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    let main = app.main_window_id.unwrap();
    let child = app.__test_seed_child_window(&["child"]);
    let sibling = app.__test_seed_child_window(&["sibling"]);
    for owner in [main, child, sibling] {
        app.open_tab_selector(owner);
        for source in [main, child, sibling] {
            assert_eq!(app.command_palette_owns_input(source), source == owner);
            let before = app.command_palette.query().to_string();
            let consumed = app.command_palette_handle_ime_in_window(
                source,
                &winit::event::Ime::Commit("x".into()),
            );
            assert_eq!(consumed, source == owner);
            if source != owner {
                assert_eq!(app.command_palette.query(), before);
            }
        }
        app.command_palette.close();
    }
}

/// Modal interception leaves a preexisting grid gesture latched and only consumes otherwise unowned motion.
#[test]
fn palette_pointer_event_preserves_existing_gesture_ownership() {
    use winit::event::{DeviceId, ElementState, MouseButton, WindowEvent};
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let pane = app.__test_seed_tab("main");
    let window_id = app.main_window_id.unwrap();
    app.open_tab_selector(window_id);
    let release = WindowEvent::MouseInput {
        device_id: DeviceId::dummy(),
        state: ElementState::Released,
        button: MouseButton::Left,
    };
    app.main_mut().unwrap().begin_pointer_press(
        super::super::PointerCell { pane_id: pane, row: 0, col: 0 },
        sonicterm_vt::vt::MouseTracking::Button,
        true,
    );
    assert!(!app.command_palette_handle_pointer_event(window_id, &release));
    assert!(app.main().unwrap().pointer_gesture.is_some());
    app.main_mut().unwrap().pointer_gesture = None;
    app.main_mut().unwrap().mouse_down = true;
    assert!(!app.command_palette_handle_pointer_event(window_id, &release));
    app.main_mut().unwrap().mouse_down = false;
    assert!(app.command_palette_handle_pointer_event(window_id, &release));
    assert!(app.command_palette.is_open());
    let child = app.__test_seed_child_window(&["child"]);
    assert!(!app.command_palette_handle_pointer_event(child, &release));
    assert!(!app.command_palette_handle_pointer_event(window_id, &WindowEvent::Focused(false)));
    assert!(app.__test_pty_write_log().is_empty());
}

/// A missing selection keeps Copy visible without dispatching it or closing the palette.
#[test]
fn disabled_copy_keeps_the_palette_open_and_clipboard_unchanged() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    app.__test_set_memory_clipboard("keep clipboard");
    assert!(app.run_action(&Action::OpenCommandPalette));
    app.__test_set_palette_query("Copy to Clipboard");
    assert!(app.__test_command_palette_handle_key(&Key::Named(NamedKey::Enter)));
    assert!(app.__test_palette_open());
    assert_eq!(app.__test_memory_clipboard().as_deref(), Some("keep clipboard"));
}

/// Copy availability revalidates selected cells and does not wait for a contended parser.
#[test]
fn palette_copy_context_rejects_replaced_selection_and_busy_parser() {
    use sonicterm_ui::command_label::DisabledReason;
    use sonicterm_ui::selection::Selection;
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let pane_id = app.__test_seed_tab("main");
    let parser = app.main().unwrap().panes[&pane_id].parser.clone();
    let selection = {
        let mut parser = parser.lock();
        parser.advance(b"copy me");
        let grid = parser.grid();
        let mut selection = Selection::new(0, 0);
        selection.extend(0, 3);
        selection
            .with_content_state(
                pane_id,
                grid.content_seq(),
                grid.is_alt(),
                grid.scrollback_evicted(),
            )
            .with_content_fingerprint(grid)
    };
    app.main_mut().unwrap().selection = Some(selection);
    assert!(app.run_action(&Action::OpenCommandPalette));
    app.__test_set_palette_query("Copy to Clipboard");
    app.refresh_command_palette_context();
    assert_eq!(
        app.command_palette.current(),
        Some(&PaletteEntry::Command(Action::CopyToClipboard))
    );
    parser.lock().advance(b"\rcopy me");
    app.refresh_command_palette_context();
    assert_eq!(
        app.command_palette.current(),
        Some(&PaletteEntry::Command(Action::CopyToClipboard))
    );
    let guard = parser.lock();
    assert!(
        super::command_palette_context(app.main().unwrap(), Some(guard.grid())).selection_available
    );
    app.refresh_command_palette_context();
    assert_eq!(
        app.command_palette.disabled_reason_for_visible_index(app.command_palette.selected()),
        Some(DisabledReason::NoSelection)
    );
    drop(guard);
    parser.lock().advance(b"\rnew text");
    app.refresh_command_palette_context();
    assert!(app.command_palette.current().is_none());
    app.__test_set_memory_clipboard("keep changed content out");
    assert!(app.__test_command_palette_handle_key(&Key::Named(NamedKey::Enter)));
    assert!(app.__test_palette_open());
    assert_eq!(app.__test_memory_clipboard().as_deref(), Some("keep changed content out"));
}

/// Palette availability follows its attached child, not a different frontmost window's tab count.
#[test]
fn palette_context_keeps_its_attached_window_and_rejects_vanished_tabs() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    let child = app.__test_seed_child_window(&["one", "two"]);
    app.__test_set_frontmost_window(Some(child));
    assert!(app.run_action(&Action::OpenCommandPalette));
    app.__test_set_frontmost_window(app.main_window_id);
    app.__test_set_palette_query("Activate Tab 2");
    assert!(app.__test_command_palette_handle_key(&Key::Named(NamedKey::ArrowLeft)));
    assert_eq!(app.command_palette.current(), Some(&PaletteEntry::Command(Action::ActivateTab(1))));
    assert_eq!(app.__test_palette_attached_window(), Some(child));
    assert!(app.__test_remove_window(child));
    assert!(app.__test_command_palette_handle_key(&Key::Named(NamedKey::Enter)));
    assert!(app.__test_palette_open());
    assert_eq!(app.main_tabs().unwrap().len(), 1);
    assert!(app.command_palette.current().is_none());
}

/// READONLY on the attached child blocks destructive palette commands before source dispatch.
#[test]
fn readonly_palette_cannot_close_a_tab_or_enter_a_mutating_submode() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    let child = app.__test_seed_child_window(&["one", "two"]);
    app.__test_set_frontmost_window(Some(child));
    assert!(app.run_action(&Action::OpenCommandPalette));
    app.windows.get_mut(&child).unwrap().copy_mode =
        Some(sonicterm_ui::copy_mode::CopyModeState::read_only_at((0, 0)));
    app.__test_set_frontmost_window(app.main_window_id);
    for query in ["Close Tab", "Rename Active Tab", "Update Tab Color"] {
        app.__test_set_palette_query(query);
        assert!(app.__test_command_palette_handle_key(&Key::Named(NamedKey::Enter)));
        assert!(app.__test_palette_open());
        assert_eq!(
            app.command_palette.mode(),
            sonicterm_ui::command_palette::CommandPaletteMode::Commands
        );
        assert_eq!(app.windows.get(&child).unwrap().tabs.len(), 2);
    }
}
