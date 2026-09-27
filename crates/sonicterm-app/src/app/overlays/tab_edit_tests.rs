use super::{theme_tab_color_choices, App};
use sonicterm_cfg::config::Config;
use sonicterm_cfg::keymap::{Action, Keymap};
use sonicterm_cfg::theme::Theme;
use sonicterm_ui::command_palette::{CommandPaletteMode, PaletteEntry};
use winit::keyboard::{Key, NamedKey};

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

/// Which window hosts a tab editor under test.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EditHost {
    Main,
    Child,
}

/// One way to open a tab editor: host window, editor kind, and entry point.
#[derive(Clone, Copy, Debug)]
struct EditCase {
    host: EditHost,
    color: bool,
    palette: bool,
}

/// Every host, editor, and entry point the tab-edit identity contract covers.
fn edit_cases() -> Vec<EditCase> {
    let mut cases = Vec::new();
    for host in [EditHost::Main, EditHost::Child] {
        for color in [false, true] {
            for palette in [false, true] {
                cases.push(EditCase { host, color, palette });
            }
        }
    }
    cases
}

/// Seed `titles` in the case's host, activate `edited`, and open its editor there.
///
/// A child host keeps an untouched main window, so a fallback to main would be visible.
fn open_tab_editor(
    case: EditCase,
    titles: &[&str],
    edited: usize,
) -> (App, winit::window::WindowId, sonicterm_ui::tabs::TabId) {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let window = match case.host {
        EditHost::Main => {
            for title in titles {
                app.__test_seed_tab(title);
            }
            app.main_window_id.unwrap()
        }
        EditHost::Child => {
            app.__test_seed_tab("main");
            app.__test_seed_child_window(titles)
        }
    };
    let host = app.windows.get_mut(&window).unwrap();
    host.tabs.activate(edited);
    let tab = host.tabs.active().unwrap().id;
    // Headless main has no native handle, so recording its id as frontmost would classify it as
    // a child. Main leaves focus unset so the editor stays unattached, as it does on real main.
    let attached = match case.host {
        EditHost::Main => None,
        EditHost::Child => {
            app.__test_set_frontmost_window(Some(window));
            Some(window)
        }
    };
    let action = if case.color { Action::UpdateTabColor } else { Action::RenameTab };
    if case.palette {
        assert!(app.run_action(&Action::OpenCommandPalette));
        app.__test_set_palette_query(if case.color { "Update Tab Color" } else { "Rename Tab" });
        assert_eq!(app.command_palette.current(), Some(&PaletteEntry::Command(action)));
        assert!(app.__test_command_palette_handle_key(&Key::Named(NamedKey::Enter)));
    } else if case.host == EditHost::Child {
        assert!(app.run_action_for_window(&action, window));
    } else {
        // The shared dispatcher keeps focus unset; explicit dispatch would record main.
        assert!(app.run_action(&action));
    }
    let expected =
        if case.color { CommandPaletteMode::TabColor } else { CommandPaletteMode::RenameTab };
    assert_eq!(app.command_palette.mode(), expected, "{case:?}");
    assert_eq!(app.palette_attached_window, attached, "{case:?}: the editor's attachment");
    (app, window, tab)
}

/// Submit the open tab editor: pick the first theme color after Reset, or type a new name.
fn submit_tab_edit(app: &mut App, case: EditCase) {
    if case.color {
        assert!(app.__test_command_palette_handle_key(&Key::Named(NamedKey::ArrowDown)));
    } else {
        app.command_palette.set_query("renamed");
    }
    assert!(app.__test_command_palette_handle_key(&Key::Named(NamedKey::Enter)));
    assert!(!app.command_palette.is_open(), "{case:?}: submit closes the editor");
}

/// Every tab in every window that carries a custom title or color.
fn edited_tabs(app: &App) -> Vec<sonicterm_ui::tabs::TabId> {
    app.windows
        .values()
        .flat_map(|window| window.tabs.tabs())
        .filter(|tab| tab.custom_title.is_some() || tab.custom_color.is_some())
        .map(|tab| tab.id)
        .collect()
}

/// Assert that a fail-closed submit left every tab in every window unchanged.
fn assert_no_tab_edited(app: &App, case: EditCase) {
    let edited = edited_tabs(app);
    assert!(edited.is_empty(), "{case:?}: fail-closed submit edited {edited:?}");
}

/// Give a headless transfer destination the geometry `transfer_tab` requires, by its role.
///
/// Test windows have no renderer, so without it the transfer refuses with `TargetNotReady`.
fn give_transfer_geometry(app: &mut App, window: winit::window::WindowId) {
    let outer = sonicterm_ui::pane::Rect::new(0.0, 0.0, 800.0, 500.0);
    if app.main_window_id == Some(window) {
        assert!(app.__test_set_main_pane_viewport(outer, 10.0, 20.0));
    } else {
        assert!(app.__test_set_child_pane_viewport(window, outer, 10.0, 20.0));
    }
}

/// Without interleaving, a tab edit changes exactly the tab it was opened for.
#[test]
fn tab_edit_changes_exactly_its_captured_tab() {
    for case in edit_cases() {
        let (mut app, window, tab) = open_tab_editor(case, &["earlier", "edited", "survivor"], 1);
        submit_tab_edit(&mut app, case);
        assert_eq!(edited_tabs(&app), vec![tab], "{case:?}");
        let edited = app.windows[&window].tabs.tabs().iter().find(|t| t.id == tab).unwrap();
        if case.color {
            assert!(edited.custom_color.is_some(), "{case:?}");
        } else {
            assert_eq!(edited.custom_title.as_deref(), Some("renamed"), "{case:?}");
        }
    }
}

/// A clean shell exit that closes the edited tab leaves its editor nothing to change.
#[test]
fn tab_edit_after_a_clean_exit_of_its_tab_changes_nothing() {
    for case in edit_cases() {
        let (mut app, window, tab) = open_tab_editor(case, &["earlier", "edited", "survivor"], 1);
        let pane = app.windows[&window].tab_states[1].active_pane;
        app.handle_pane_process_exited(pane, Some(true));
        let tabs = &app.windows[&window].tabs;
        assert!(tabs.tabs().iter().all(|t| t.id != tab), "{case:?}: the exit closes its tab");
        assert_eq!(tabs.active().map(|t| t.title.as_str()), Some("survivor"), "{case:?}");
        submit_tab_edit(&mut app, case);
        assert_no_tab_edited(&app, case);
    }
}

/// Closing an earlier tab shifts indices, but the editor still changes only its own tab.
#[test]
fn tab_edit_follows_its_tab_after_an_earlier_tab_closes() {
    for case in edit_cases() {
        let (mut app, window, tab) = open_tab_editor(case, &["earlier", "edited", "survivor"], 1);
        match case.host {
            EditHost::Main => app.close_tab_at(0),
            EditHost::Child => assert!(app.close_tab_at_in_child(window, 0)),
        }
        submit_tab_edit(&mut app, case);
        assert_eq!(edited_tabs(&app), vec![tab], "{case:?}");
    }
}

/// Reordering moves the edited tab's slot, but the editor still follows its identity.
#[test]
fn tab_edit_follows_its_tab_across_a_reorder() {
    for case in edit_cases() {
        let (mut app, window, tab) = open_tab_editor(case, &["earlier", "edited", "survivor"], 1);
        let host = app.windows.get_mut(&window).unwrap();
        host.tabs.reorder(1, 2);
        host.tab_states.swap(1, 2);
        host.tabs.reorder(0, 1);
        host.tab_states.swap(0, 1);
        assert_eq!(host.tabs.tabs()[2].id, tab);
        submit_tab_edit(&mut app, case);
        assert_eq!(edited_tabs(&app), vec![tab], "{case:?}");
    }
}

/// Switching the active tab by a tab-bar press or a menu action never redirects an open editor.
#[test]
fn tab_edit_ignores_a_pointer_or_menu_tab_switch() {
    for case in edit_cases() {
        for by_menu in [false, true] {
            let (mut app, window, tab) =
                open_tab_editor(case, &["earlier", "edited", "survivor"], 1);
            if by_menu {
                assert!(app.run_action_for_window(&Action::NextTab, window));
            } else if case.host == EditHost::Main {
                // A main tab-bar press activates through this helper.
                assert!(app.activate_main_tab(2));
            } else {
                // A child tab-bar press applies the same activation this helper performs.
                assert!(app.activate_tab_in_child(window, 2));
            }
            assert_ne!(app.windows[&window].tabs.active().unwrap().id, tab, "{case:?}");
            submit_tab_edit(&mut app, case);
            assert_eq!(edited_tabs(&app), vec![tab], "{case:?} by_menu={by_menu}");
        }
    }
}

/// A replacement tab with the same title, slot, and active state never receives the edit.
#[test]
fn tab_edit_never_lands_on_a_same_title_replacement() {
    for case in edit_cases() {
        let (mut app, window, tab) = open_tab_editor(case, &["earlier", "edited", "survivor"], 1);
        match case.host {
            EditHost::Main => {
                app.close_tab_at(1);
                app.__test_seed_tab("edited");
                let host = app.windows.get_mut(&window).unwrap();
                host.tabs.reorder(2, 1);
                host.tab_states.swap(1, 2);
            }
            EditHost::Child => {
                assert!(app.close_tab_at_in_child(window, 1));
                app.__test_seed_tab("edited");
                give_transfer_geometry(&mut app, window);
                app.transfer_tab(None, 1, Some(window), 1).unwrap();
            }
        }
        let tabs = &app.windows[&window].tabs;
        let replacement = tabs.active().unwrap();
        assert_ne!(replacement.id, tab);
        assert_eq!(replacement.title, "edited");
        assert_eq!(tabs.active_index(), 1);
        submit_tab_edit(&mut app, case);
        assert_no_tab_edited(&app, case);
    }
}

/// Closing the editor's source window, or hiding main, closes the editor without an edit.
#[test]
fn tab_edit_closes_with_its_source_window() {
    for case in edit_cases() {
        let (mut app, window, _) = open_tab_editor(case, &["edited", "survivor"], 0);
        match case.host {
            EditHost::Main => app.hide_main_window(),
            EditHost::Child => assert!(app.close_child_window(window)),
        }
        assert!(!app.command_palette.is_open(), "{case:?}: the editor closes with its window");
        // A stray Enter afterwards has no editor to submit.
        app.__test_command_palette_handle_key(&Key::Named(NamedKey::Enter));
        assert_no_tab_edited(&app, case);
    }
}

/// A tab moved to another window before submit fails closed instead of following it.
#[test]
fn tab_edit_fails_closed_after_its_tab_moves_to_another_window() {
    for case in edit_cases() {
        let (mut app, window, tab) = open_tab_editor(case, &["edited", "survivor"], 0);
        let destination = match case.host {
            EditHost::Main => app.__test_seed_child_window(&["other"]),
            EditHost::Child => app.main_window_id.unwrap(),
        };
        give_transfer_geometry(&mut app, destination);
        app.transfer_tab(Some(window), 0, Some(destination), 1).unwrap();
        assert!(app.windows[&destination].tabs.tabs().iter().any(|t| t.id == tab));
        assert!(app.command_palette.is_open(), "{case:?}: the source window survives the move");
        submit_tab_edit(&mut app, case);
        assert_no_tab_edited(&app, case);
    }
}
