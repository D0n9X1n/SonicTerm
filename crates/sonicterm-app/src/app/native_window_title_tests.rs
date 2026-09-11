use super::{App, LINUX_DESKTOP_ID, LINUX_INSTANCE_NAME, NATIVE_WINDOW_TITLE};
use sonicterm_cfg::{
    config::Config,
    keymap::{Action, Keymap},
    theme::Theme,
};
use winit::keyboard::{Key, NamedKey};

#[test]
fn terminal_window_numbers_survive_transfer_hide_and_never_reuse() {
    // Window identity belongs to admission, not tabs, focus, or a retained window's visibility.
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("source");
    app.__test_seed_tab("remaining");
    let main = app.main_window_id.unwrap();
    let child = app.__test_seed_child_window(&["destination"]);
    assert_eq!(app.native_window_title(main).as_deref(), Some("#1 SonicTerm"));
    assert_eq!(app.native_window_title(child).as_deref(), Some("#2 SonicTerm"));
    app.start_rename_window(child);
    app.command_palette.set_query(" Work ");
    app.__test_command_palette_handle_key(&Key::Named(NamedKey::Enter));
    app.windows.get_mut(&child).unwrap().test_pane_viewport =
        Some((sonicterm_ui::pane::Rect { x: 0.0, y: 0.0, w: 800.0, h: 500.0 }, 10.0, 20.0));
    app.transfer_tab(None, 0, Some(child), 1).unwrap();
    app.hide_main_window();
    app.show_main_window();
    assert_eq!(app.native_window_title(child).as_deref(), Some("#2 Work"));
    app.windows.remove(&child);
    app.release_child_window_registries(child);
    let next = app.__test_seed_child_window(&["new"]);
    assert_eq!(app.native_window_title(next).as_deref(), Some("#3 SonicTerm"));
    assert_eq!(app.native_window_title(main).as_deref(), Some("#1 SonicTerm"));
}

#[test]
fn window_rename_captures_main_and_child_despite_focus_changes() {
    // Duplicate names are legal; each editor changes only the captured stable window key.
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    let main = app.main_window_id.unwrap();
    let child = app.__test_seed_child_window(&["child"]);
    for target in [main, child] {
        app.run_action_for_window(&Action::RenameWindow, target);
        app.command_palette.set_query("工作");
        app.__test_set_frontmost_window(Some(if target == main { child } else { main }));
        app.__test_command_palette_handle_key(&Key::Named(NamedKey::Enter));
    }
    assert_eq!(app.native_window_title(main).as_deref(), Some("#1 工作"));
    assert_eq!(app.native_window_title(child).as_deref(), Some("#2 工作"));
    app.start_rename_window(main);
    assert_eq!(app.command_palette.query(), "工作");
    app.command_palette.set_query("cancelled");
    app.__test_command_palette_handle_key(&Key::Named(NamedKey::Escape));
    assert_eq!(app.native_window_title(main).as_deref(), Some("#1 工作"));
    app.start_rename_window(main);
    app.command_palette.set_query("   ");
    app.__test_command_palette_handle_key(&Key::Named(NamedKey::Enter));
    assert_eq!(app.native_window_title(main).as_deref(), Some("#1 SonicTerm"));
    assert!(app.__test_drain_pty_writes().is_empty());
}

#[test]
fn closed_rename_target_never_falls_back_to_replacement_main() {
    // Reusing a native id after destruction cannot revive the old stable rename key.
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    let main = app.main_window_id.unwrap();
    app.start_rename_window(main);
    app.command_palette.set_query("stale");
    app.windows.remove(&main);
    app.window_keys.remove(main);
    app.main_window_id = None;
    app.__test_seed_tab("replacement");
    app.__test_command_palette_handle_key(&Key::Named(NamedKey::Enter));
    assert!(!app.command_palette.is_open());
    assert_eq!(app.native_window_title(main).as_deref(), Some("#2 SonicTerm"));
}

#[test]
fn window_rename_ime_and_readonly_remain_local_ui() {
    // READONLY still exposes the palette; composing Enter cannot commit or write to any pane.
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    let main = app.main_window_id.unwrap();
    app.run_action_for_window(&Action::EnterCopyMode, main);
    assert!(app.__test_main_read_only());
    app.run_action_for_window(&Action::OpenCommandPalette, main);
    assert!(app.command_palette.is_open());
    app.command_palette.set_query("Rename Window");
    app.__test_command_palette_handle_key(&Key::Named(NamedKey::Enter));
    app.command_palette_handle_ime_in_window(
        main,
        &winit::event::Ime::Preedit("工作".into(), Some((0, 6))),
    );
    app.__test_command_palette_handle_key(&Key::Named(NamedKey::Enter));
    assert_eq!(app.native_window_title(main).as_deref(), Some("#1 SonicTerm"));
    app.command_palette_handle_ime_in_window(main, &winit::event::Ime::Commit("工作".into()));
    app.__test_command_palette_handle_key(&Key::Named(NamedKey::Enter));
    assert_eq!(app.native_window_title(main).as_deref(), Some("#1 工作"));
    assert!(app.__test_main_read_only());
    assert!(app.__test_drain_pty_writes().is_empty());
}

#[test]
fn window_rename_rejects_invalid_commit_without_changing_title() {
    // Validation failures leave the editor open and the last valid native title untouched.
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    let main = app.main_window_id.unwrap();
    for invalid in ["Work\n".to_string(), "界".repeat(129)] {
        app.start_rename_window(main);
        app.command_palette.set_query(invalid);
        app.__test_command_palette_handle_key(&Key::Named(NamedKey::Enter));
        assert!(app.command_palette.is_open());
        assert!(app.command_palette.window_name_error().is_some());
        assert_eq!(app.native_window_title(main).as_deref(), Some("#1 SonicTerm"));
        app.__test_command_palette_handle_key(&Key::Named(NamedKey::Escape));
    }
}

#[test]
fn last_tab_transfer_retires_child_number_but_preserves_destination() {
    // A drained child is destroyed, while the existing destination keeps its name and number.
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    let main = app.main_window_id.unwrap();
    let child = app.__test_seed_child_window(&["child"]);
    app.windows.get_mut(&main).unwrap().test_pane_viewport =
        Some((sonicterm_ui::pane::Rect { x: 0.0, y: 0.0, w: 800.0, h: 500.0 }, 10.0, 20.0));
    app.start_rename_window(main);
    app.command_palette.set_query("destination");
    app.__test_command_palette_handle_key(&Key::Named(NamedKey::Enter));
    app.transfer_tab(Some(child), 0, None, 1).unwrap();
    app.reap_empty_child(child);
    assert!(app.window_keys.get(child).is_none());
    assert_eq!(app.native_window_title(main).as_deref(), Some("#1 destination"));
    let next = app.__test_seed_child_window(&["new"]);
    assert_eq!(app.native_window_title(next).as_deref(), Some("#3 SonicTerm"));
}

#[test]
fn native_window_title_base_is_app_name() {
    // The default title stays independent of custom names and desktop application identity.
    assert_eq!(NATIVE_WINDOW_TITLE, "SonicTerm");
}

#[test]
fn linux_window_identity_matches_packaged_desktop_metadata() {
    // Protect X11 WM_CLASS and Wayland app ID from drifting away from package metadata.
    assert_eq!(LINUX_DESKTOP_ID, "com.d0n9x1n.SonicTerm");
    assert_eq!(LINUX_INSTANCE_NAME, "sonicterm");
}
