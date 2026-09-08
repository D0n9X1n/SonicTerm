use super::{theme_tab_color_choices, App};
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

#[test]
fn palette_rename_and_color_keep_their_source_after_focus_changes() {
    // Editor entry and completion must keep the palette's window rather than following new focus.
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    let main = app.main_window_id.unwrap();
    let child = app.__test_seed_child_window(&["child"]);
    app.__test_set_frontmost_window(Some(child));
    app.run_action(&Action::OpenCommandPalette);
    app.__test_set_palette_query("Rename Tab");
    app.__test_set_frontmost_window(Some(main));
    app.__test_command_palette_handle_key(&Key::Named(NamedKey::Enter));
    assert_eq!(app.command_palette.query(), "child");
    app.command_palette.set_query("renamed");
    app.__test_command_palette_handle_key(&Key::Named(NamedKey::Enter));
    assert_eq!(app.windows[&child].tabs.active_title_body().as_deref(), Some("renamed"));
    assert_eq!(app.windows[&main].tabs.active_title_body().as_deref(), Some("main"));
    app.__test_set_frontmost_window(Some(child));
    app.run_action(&Action::OpenCommandPalette);
    app.__test_set_palette_query("Update Tab Color");
    app.__test_set_frontmost_window(Some(main));
    app.__test_command_palette_handle_key(&Key::Named(NamedKey::Enter));
    assert_eq!(app.palette_attached_window, Some(child));
    app.__test_command_palette_handle_key(&Key::Named(NamedKey::ArrowDown));
    let color = app.command_palette.selected_tab_color().unwrap().hex.clone();
    assert!(color.is_some());
    app.__test_command_palette_handle_key(&Key::Named(NamedKey::Enter));
    assert_eq!(app.windows[&child].tabs.active_custom_color(), color.as_deref());
    assert_eq!(app.windows[&main].tabs.active_custom_color(), None);
}

fn pointer_target(app: &App, index: usize) -> super::PalettePointerHit {
    super::PalettePointerHit::Row { index, entry: app.command_palette.visible()[index].clone() }
}

fn press_palette_target(app: &mut App, window_id: winit::window::WindowId, index: usize) {
    let entry = app.command_palette.visible()[index].clone();
    assert!(app.command_palette.select_visible_index(index));
    app.palette_pointer_capture = Some(super::PalettePointerCapture {
        window_id,
        target: super::PalettePointerTarget::Entry(entry),
    });
}

/// Release resolves the pressed child target by ID even after a reorder without an intervening redraw.
#[test]
fn palette_pointer_release_revalidates_attached_tab_identity() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    let child = app.__test_seed_child_window(&["same", "same", "peer"]);
    let target = app.windows[&child].tabs.tabs()[0].id;
    app.open_tab_selector(child);
    press_palette_target(&mut app, child, 0);
    let release = pointer_target(&app, 0);
    let window = app.windows.get_mut(&child).unwrap();
    window.tabs.reorder(0, 1);
    window.tab_states.swap(0, 1);
    window.tabs.activate(2);
    app.__test_set_frontmost_window(app.main_window_id);
    app.release_command_palette_pointer(child, Some(release));
    assert_eq!(app.windows[&child].tabs.active().unwrap().id, target);
    assert!(!app.command_palette.is_open());
    assert!(app.palette_pointer_capture.is_none());
}

/// A redraw that moves another row under the held pointer cancels activation instead of retargeting it.
#[test]
fn palette_pointer_release_rejects_a_different_displayed_row() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("first");
    app.__test_seed_tab("second");
    app.__test_seed_tab("active");
    let window_id = app.main_window_id.unwrap();
    let active = app.main_tabs().unwrap().active().unwrap().id;
    app.open_tab_selector(window_id);
    press_palette_target(&mut app, window_id, 0);
    let window = app.windows.get_mut(&window_id).unwrap();
    window.tabs.reorder(0, 1);
    window.tab_states.swap(0, 1);
    app.refresh_command_palette_context();
    let release = pointer_target(&app, 0);
    app.release_command_palette_pointer(window_id, Some(release));
    assert_eq!(app.main_tabs().unwrap().active().unwrap().id, active);
    assert!(app.command_palette.is_open());
}

/// Closed targets cannot lend their pointer capture to a same-title replacement at the old slot.
#[test]
fn palette_pointer_release_rejects_a_closed_target() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("same");
    app.__test_seed_tab("active");
    let window_id = app.main_window_id.unwrap();
    let target = app.main_tabs().unwrap().tabs()[0].id;
    let active = app.main_tabs().unwrap().active().unwrap().id;
    app.open_tab_selector(window_id);
    press_palette_target(&mut app, window_id, 0);
    let release = pointer_target(&app, 0);
    let window = app.windows.get_mut(&window_id).unwrap();
    window.tabs.close(target);
    window.tab_states.remove(0);
    app.__test_seed_tab("same");
    let window = app.windows.get_mut(&window_id).unwrap();
    window.tabs.activate(0);
    app.release_command_palette_pointer(window_id, Some(release));
    assert_eq!(app.main_tabs().unwrap().active().unwrap().id, active);
    assert!(app.command_palette.is_open());
    assert!(app.command_palette.current().is_none());
}

/// Disabled clicks keep the modal open, while outside dismissal consumes its own paired release.
#[test]
fn palette_pointer_disabled_and_outside_clicks_never_dispatch() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    let window_id = app.main_window_id.unwrap();
    assert!(app.run_action(&Action::OpenCommandPalette));
    app.__test_set_palette_query("Copy to Clipboard");
    app.__test_set_memory_clipboard("unchanged");
    press_palette_target(&mut app, window_id, 0);
    let release = pointer_target(&app, 0);
    app.release_command_palette_pointer(window_id, Some(release));
    assert!(app.command_palette.is_open());
    assert_eq!(app.__test_memory_clipboard().as_deref(), Some("unchanged"));
    app.palette_pointer_capture = Some(super::PalettePointerCapture {
        window_id,
        target: super::PalettePointerTarget::Outside,
    });
    app.release_command_palette_pointer(window_id, Some(super::PalettePointerHit::Outside));
    assert!(!app.command_palette.is_open());
    assert!(app.__test_pty_write_log().is_empty());
}

/// Keyboard and IME edits revoke a held row so its later release cannot run a newly selected entry.
#[test]
fn palette_pointer_capture_is_cleared_by_keyboard_ime_and_reopen() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("first");
    app.__test_seed_tab("active");
    let window_id = app.main_window_id.unwrap();
    for edit in 0..3 {
        app.open_tab_selector(window_id);
        press_palette_target(&mut app, window_id, 0);
        match edit {
            0 => {
                app.command_palette_handle_logical_key(&Key::Named(NamedKey::ArrowDown));
            }
            1 => {
                app.command_palette_handle_ime(&winit::event::Ime::Preedit(
                    "composition".into(),
                    None,
                ));
            }
            _ => {
                app.open_tab_selector(window_id);
            }
        }
        assert!(app.palette_pointer_capture.is_none());
        app.main_mut().unwrap().ime.cancel();
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

/// Wheel input stays bounded at the list ends and never reaches a PTY or a noncommand picker.
#[test]
fn palette_pointer_wheel_is_bounded_and_modal() {
    use winit::event::{DeviceId, MouseScrollDelta, TouchPhase, WindowEvent};
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("first");
    app.__test_seed_tab("second");
    app.__test_seed_tab("last");
    let window_id = app.main_window_id.unwrap();
    app.open_tab_selector(window_id);
    let wheel = |y| WindowEvent::MouseWheel {
        device_id: DeviceId::dummy(),
        delta: MouseScrollDelta::LineDelta(0.0, y),
        phase: TouchPhase::Moved,
    };
    assert!(app.command_palette_handle_pointer_event(window_id, &wheel(f32::MAX)));
    assert_eq!(app.command_palette.selected(), 0);
    assert!(app.command_palette_handle_pointer_event(window_id, &wheel(-f32::MAX)));
    assert_eq!(app.command_palette.selected(), 1);
    for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, 0.0] {
        assert!(app.command_palette_handle_pointer_event(window_id, &wheel(value)));
        assert_eq!(app.command_palette.selected(), 1);
    }
    for _ in 0..4 {
        app.command_palette_handle_pointer_event(window_id, &wheel(-1.0));
    }
    assert_eq!(app.command_palette.selected(), 2);
    app.command_palette.start_rename_tab("literal");
    assert!(app.command_palette_handle_pointer_event(window_id, &wheel(-1.0)));
    assert_eq!(app.command_palette.query(), "literal");
    assert!(app.__test_pty_write_log().is_empty());
}

/// Closing the host or changing attachment cannot send a captured click into another window.
#[test]
fn palette_pointer_capture_does_not_cross_window_ownership() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("same");
    let main_id = app.main_window_id.unwrap();
    let main_tab = app.main_tabs().unwrap().active().unwrap().id;
    let child = app.__test_seed_child_window(&["same", "active"]);
    app.open_tab_selector(child);
    press_palette_target(&mut app, child, 0);
    let release = pointer_target(&app, 0);
    app.__test_remove_window(child);
    app.release_command_palette_pointer(child, Some(release));
    assert_eq!(app.main_tabs().unwrap().active().unwrap().id, main_tab);
    assert!(app.command_palette.is_open());
    app.open_tab_selector(main_id);
    press_palette_target(&mut app, main_id, 0);
    let release = pointer_target(&app, 0);
    app.release_command_palette_pointer(child, Some(release));
    assert!(app.command_palette.is_open());
    assert_eq!(app.main_tabs().unwrap().active().unwrap().id, main_tab);
}

/// Go to Tab resolves the same child TabId after reorder even when another window is frontmost.
#[test]
fn go_to_tab_activates_stable_identity_in_its_attached_window() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("target");
    let child = app.__test_seed_child_window(&["target", "peer"]);
    let target = app.windows[&child].tabs.tabs()[0].id;
    app.__test_set_frontmost_window(Some(child));
    assert!(app.run_action(&Action::OpenCommandPalette));
    app.__test_set_palette_query("target");
    assert!(app.command_palette.current().is_some(), "the tab target must be searchable");
    let child_state = app.windows.get_mut(&child).unwrap();
    child_state.tabs.reorder(0, 1);
    child_state.tab_states.swap(0, 1);
    child_state.tabs.activate(0);
    app.__test_set_frontmost_window(app.main_window_id);
    assert!(app.__test_command_palette_handle_key(&Key::Named(NamedKey::Enter)));
    assert_eq!(app.windows[&child].tabs.active().unwrap().id, target);
    assert_eq!(app.main_tabs().unwrap().tabs()[0].title, "target");
    assert!(!app.__test_palette_open());
}

/// Closing the selected target before Enter must not activate the tab that inherits its numeric slot.
#[test]
fn go_to_tab_closed_target_does_not_activate_a_replacement() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    let child = app.__test_seed_child_window(&["same", "same", "other"]);
    let target = app.windows[&child].tabs.tabs()[0].id;
    let active = app.windows[&child].tabs.active().unwrap().id;
    app.__test_set_frontmost_window(Some(child));
    assert!(app.run_action(&Action::OpenCommandPalette));
    app.__test_set_palette_query("Go to Tab 1: same");
    assert!(app.command_palette.current().is_some());
    let child_state = app.windows.get_mut(&child).unwrap();
    child_state.tabs.close(target);
    child_state.tab_states.remove(0);
    app.refresh_command_palette_context();
    assert!(
        app.command_palette.current().is_none(),
        "a redraw-equivalent refresh leaves no replacement selected"
    );
    assert!(app.__test_command_palette_handle_key(&Key::Named(NamedKey::Enter)));
    assert!(app.__test_palette_open());
    assert_eq!(app.windows[&child].tabs.active().unwrap().id, active);
    assert!(app.command_palette.current().is_none());
}

/// A closed host window removes its tab targets without borrowing matching titles from main.
#[test]
fn go_to_tab_vanished_child_never_activates_main() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("same target");
    let main_tab = app.main_tabs().unwrap().active().unwrap().id;
    let child = app.__test_seed_child_window(&["same target"]);
    let child_tab = app.windows[&child].tabs.active().unwrap().id;
    app.__test_set_frontmost_window(Some(child));
    assert!(app.run_action(&Action::OpenCommandPalette));
    app.__test_set_palette_query("same target");
    assert!(
        matches!(app.command_palette.current(), Some(PaletteEntry::Tab { id, .. }) if *id == child_tab)
    );
    assert!(app.__test_remove_window(child));
    assert!(app.__test_command_palette_handle_key(&Key::Named(NamedKey::Enter)));
    assert!(app.__test_palette_open());
    assert_eq!(app.main_tabs().unwrap().active().unwrap().id, main_tab);
    assert!(!app
        .command_palette
        .visible()
        .iter()
        .any(|entry| matches!(entry, PaletteEntry::Tab { .. })));
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

#[test]
fn tab_color_choices_include_reset_and_only_ansi_colors() {
    let theme = Theme::default();
    let bg = theme.colors.background.0.to_ascii_lowercase();
    let choices = theme_tab_color_choices(&theme);

    assert_eq!(choices.first().map(|choice| choice.name.as_str()), Some("Reset to Default"));
    assert_eq!(choices.first().and_then(|choice| choice.hex.as_deref()), None);
    assert_eq!(choices.len(), 17);
    assert!(choices
        .iter()
        .skip(1)
        .all(|choice| choice.name.starts_with("ANSI ") || choice.name.starts_with("Bright ")));
    assert!(choices
        .iter()
        .filter_map(|choice| choice.hex.as_ref())
        .all(|hex| hex.to_ascii_lowercase() != bg));
}
