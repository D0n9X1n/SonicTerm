use super::App;
use sonicterm_cfg::config::Config;
use sonicterm_cfg::keymap::{Action, Keymap};
use sonicterm_cfg::theme::Theme;
use sonicterm_ui::command_palette::PaletteEntry;
use winit::keyboard::{Key, NamedKey};

#[cfg(target_os = "macos")]
#[test]
fn native_mac_deletion_stays_in_the_attached_palette_editor() {
    // Command, Option and Control edits follow the source editor, including rename modes and Unicode decomposition.
    use winit::keyboard::ModifiersState;
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    let main = app.main_window_id.unwrap();
    let child = app.__test_seed_child_window(&["child"]);
    app.__test_enable_pty_write_log();
    for (owner, other) in [(main, child), (child, main)] {
        for rename in [false, true] {
            for (mods, before, after) in [
                (ModifiersState::SUPER, "alpha beta", ""),
                (ModifiersState::ALT, "alpha beta", "alpha "),
                (ModifiersState::CONTROL, "alpha é", "alpha e"),
            ] {
                app.run_action_for_window(&Action::OpenCommandPalette, owner);
                if rename {
                    app.command_palette.start_rename_tab(before);
                } else {
                    app.command_palette.set_query(before);
                }
                app.windows.get_mut(&owner).unwrap().modifiers = mods;
                app.frontmost_window = Some(other);
                assert!(app.command_palette_handle_logical_key(&Key::Named(NamedKey::Backspace)));
                assert_eq!(app.command_palette.query(), after);
                assert_eq!(app.frontmost_window, Some(other));
                assert!(app.__test_drain_pty_writes().is_empty());
                app.command_palette.close();
            }
        }
    }
}

#[test]
fn modified_backspace_never_falls_through_to_plain_palette_deletion() {
    // Unsupported extra modifiers and active composition cannot erase already committed palette or rename text.
    use winit::keyboard::ModifiersState;
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    let main = app.main_window_id.unwrap();
    let child = app.__test_seed_child_window(&["child"]);
    app.__test_enable_pty_write_log();
    for owner in [main, child] {
        for rename in [false, true] {
            app.run_action_for_window(&Action::OpenCommandPalette, owner);
            if rename {
                app.command_palette.start_rename_tab("keep é");
            } else {
                app.command_palette.set_query("keep é");
            }
            app.windows.get_mut(&owner).unwrap().modifiers =
                ModifiersState::CONTROL | ModifiersState::ALT;
            assert!(app.command_palette_handle_logical_key(&Key::Named(NamedKey::Backspace)));
            assert_eq!(app.command_palette.query(), "keep é");
            app.windows.get_mut(&owner).unwrap().modifiers = ModifiersState::CONTROL;
            app.windows.get_mut(&owner).unwrap().ime.handle_preedit("ni", None);
            assert!(app.command_palette_handle_logical_key(&Key::Named(NamedKey::Backspace)));
            assert_eq!(app.command_palette.query(), "keep é");
            assert!(app.__test_drain_pty_writes().is_empty());
            app.windows.get_mut(&owner).unwrap().ime.cancel();
            // Shift alone retains plain deletion when the modifier outlives a typed capital.
            app.windows.get_mut(&owner).unwrap().modifiers = ModifiersState::SHIFT;
            assert!(app.command_palette_handle_logical_key(&Key::Named(NamedKey::Backspace)));
            assert_eq!(app.command_palette.query(), "keep ");
            assert!(app.__test_drain_pty_writes().is_empty());
            app.command_palette.close();
        }
    }
}

/// About uses the originating window's green notification even if focus moves before activation.
#[test]
fn about_palette_shows_compiled_version_in_source_window_notification() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    let main = app.main_window_id.unwrap();
    let child = app.__test_seed_child_window(&["child"]);
    for (owner, other) in [(main, child), (child, main)] {
        app.__test_set_frontmost_window(Some(owner));
        app.run_action_for_window(&Action::OpenCommandPalette, owner);
        app.__test_set_palette_query("About SonicTerm");
        assert_eq!(app.command_palette.current(), Some(&PaletteEntry::About));
        app.__test_set_frontmost_window(Some(other));
        let before = std::time::Instant::now();
        assert!(app.command_palette_handle_logical_key(&Key::Named(NamedKey::Enter)));
        let after = std::time::Instant::now();
        assert!(!app.command_palette.is_open());
        assert_eq!(app.palette_attached_window, None);
        assert_eq!(app.frontmost_window, Some(other));
        let bubble = app.windows[&owner].notification.as_ref().expect("About notification");
        assert_eq!(bubble.level, sonicterm_ui::overlays::NotificationLevel::Info);
        assert_eq!(bubble.message, format!("SonicTerm {}", env!("CARGO_PKG_VERSION")));
        let expires = bubble.expires_at.expect("standard notification expiry");
        assert!(expires >= before + std::time::Duration::from_secs(5));
        assert!(expires <= after + std::time::Duration::from_secs(5));
        assert!(app.windows[&other].notification.is_none());
        assert!(app.__test_drain_pty_writes().is_empty());
        app.expire_notifications(expires);
        assert!(app.windows[&owner].notification.is_none());
    }
}

/// About remains available in READONLY and releases the palette without changing terminal mode or input.
#[test]
fn about_palette_notification_preserves_readonly_and_reopens_normally() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    let window_id = app.main_window_id.unwrap();
    app.windows.get_mut(&window_id).unwrap().copy_mode =
        Some(sonicterm_ui::copy_mode::CopyModeState::read_only_at((0, 0)));
    app.run_action_for_window(&Action::OpenCommandPalette, window_id);
    app.__test_set_palette_query("version");
    app.refresh_command_palette_context();
    let index = app
        .command_palette
        .visible()
        .iter()
        .position(|entry| **entry == PaletteEntry::About)
        .unwrap();
    assert!(app.command_palette.select_visible_index(index));
    assert_eq!(app.command_palette.current(), Some(&PaletteEntry::About));
    app.command_palette_handle_logical_key(&Key::Named(NamedKey::Enter));
    assert!(!app.command_palette.is_open());
    assert_eq!(
        app.__test_main_notification_message(),
        Some(format!("SonicTerm {}", env!("CARGO_PKG_VERSION")).as_str())
    );
    assert!(app.windows[&window_id].copy_mode.as_ref().unwrap().is_read_only());
    assert!(app.__test_drain_pty_writes().is_empty());
    app.run_action_for_window(&Action::OpenCommandPalette, window_id);
    assert!(app.command_palette.is_open());
    app.command_palette_handle_logical_key(&Key::Character("a".into()));
    assert_eq!(app.command_palette.query(), "a");
    app.command_palette_handle_logical_key(&Key::Named(NamedKey::Escape));
    assert!(!app.command_palette.is_open());
    assert!(app.__test_drain_pty_writes().is_empty());
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
