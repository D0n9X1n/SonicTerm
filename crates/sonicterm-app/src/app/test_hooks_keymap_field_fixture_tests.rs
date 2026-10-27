//! Palette and search field clipboard fixtures and routing contracts. regression contracts.

use super::*;

/// Commands and RenameTab fields own configured copy and paste before search, READONLY, and terminal delivery.
#[cfg(any(windows, unix))]
#[test]
fn real_pty_palette_field_clipboard_owns_explicit_and_menu_actions() {
    use sonicterm_ui::{command_palette::CommandPaletteMode, text_edit::TextEdit};
    if isolated() {
        return;
    }
    for owner_index in 0..2 {
        // The fixture has already proven that this owner's paste reaches its AllTabs peers.
        let (mut app, windows, submitted) = window_name_paste_fixture(owner_index);
        let (owner, owner_pane) = windows[owner_index];
        let (unrelated, _) = windows[(owner_index + 2) % 3];
        seed_terminal_selection(&mut app, owner, owner_pane);
        for mode in [CommandPaletteMode::Commands, CommandPaletteMode::RenameTab] {
            for menu in [false, true] {
                for (read_only, search_open) in [(false, false), (true, true)] {
                    let case = format!(
                        "owner={owner_index} mode={mode:?} menu={menu} read_only={read_only}"
                    );
                    open_field_palette(&mut app, owner, mode);
                    configure_window_name_underlay(
                        &mut app,
                        &windows,
                        owner,
                        read_only,
                        search_open,
                    );
                    // Cached focus names the owner only for the menu path; explicit dispatch must ignore it.
                    app.frontmost_window = Some(if menu { owner } else { unrelated });
                    app.command_palette.set_query("你好");
                    app.command_palette.apply_text_edit(TextEdit::MoveStart);
                    app.command_palette.apply_text_edit(TextEdit::MoveForward);
                    app.__test_set_memory_clipboard("é");
                    app.wait_for_input_queues();
                    dispatch_field_test_action(&mut app, &Action::PasteFromClipboard, owner, menu);
                    assert_eq!(app.command_palette.query(), "你é好", "{case}");
                    assert_eq!(app.command_palette.cursor(), "你é".len(), "{case}");
                    assert_eq!(
                        (app.__test_drain_pty_writes(), submitted.take()),
                        (Vec::new(), Vec::new()),
                        "{case}"
                    );

                    // Copy writes only the selected Unicode field text and keeps the selection visible.
                    app.command_palette.extend_to("你é好".len());
                    dispatch_field_test_action(&mut app, &Action::CopyToClipboard, owner, menu);
                    assert_eq!(app.__test_memory_clipboard().as_deref(), Some("好"), "{case}");
                    assert_eq!(app.command_palette.selected_text(), Some("好"), "{case}");

                    // Without a field selection, copy is a no-op rather than the terminal selection underneath.
                    app.command_palette.set_cursor(0);
                    app.__test_set_memory_clipboard("sentinel");
                    dispatch_field_test_action(&mut app, &Action::CopyToClipboard, owner, menu);
                    assert_eq!(
                        app.__test_memory_clipboard().as_deref(),
                        Some("sentinel"),
                        "{case}"
                    );
                    assert!(app.windows[&owner].selection.is_some(), "{case}");
                    assert_eq!(app.command_palette.mode(), mode, "{case}");
                    if search_open {
                        let search = app.windows[&owner].tab_states[0].search.as_ref().unwrap();
                        assert_eq!(search.query, "underlay", "{case}");
                    }
                    assert_eq!(
                        (app.__test_drain_pty_writes(), submitted.take()),
                        (Vec::new(), Vec::new()),
                        "{case}"
                    );
                    assert!(app.windows[&unrelated].notification.is_none(), "{case}");
                }
            }
        }
        close_field_palette(&mut app);
    }
}

/// Failed clipboard reads or writes, composition, TabColor, and stale or hidden targets stay local; unrelated sources keep terminal paste.
#[cfg(any(windows, unix))]
#[test]
fn real_pty_palette_field_clipboard_failures_and_stale_targets() {
    use sonicterm_ui::command_palette::CommandPaletteMode;
    if isolated() {
        return;
    }
    for owner_index in 0..2 {
        let (mut app, windows, submitted) = window_name_paste_fixture(owner_index);
        let (owner, _) = windows[owner_index];
        let (unrelated, unrelated_pane) = windows[(owner_index + 2) % 3];
        open_field_palette(&mut app, owner, CommandPaletteMode::RenameTab);
        app.command_palette.set_query("你好");
        app.command_palette.select_all();
        let whole = Some(0.."你好".len());

        // A rejected clipboard write keeps the field text, its selection, and the prior clipboard.
        app.__test_set_memory_clipboard("sentinel");
        app.__test_set_clipboard_write_failure(true);
        assert!(app.run_action_for_window(&Action::CopyToClipboard, owner));
        app.__test_set_clipboard_write_failure(false);
        assert_eq!(app.__test_memory_clipboard().as_deref(), Some("sentinel"));
        assert_eq!(app.command_palette.selected_range(), whole);

        // Empty, control-only, and unreadable clipboards are consumed without any fallback delivery.
        for text in [Some(""), Some("\r\n\x1b\x7f"), None] {
            app.test_clipboard_text = text.map(str::to_owned);
            app.clipboard = None;
            app.wait_for_input_queues();
            assert!(app.run_action_for_window(&Action::PasteFromClipboard, owner));
            assert_eq!(app.command_palette.query(), "你好", "{text:?}");
            assert_eq!(app.command_palette.selected_range(), whole, "{text:?}");
            assert_eq!((app.__test_drain_pty_writes(), submitted.take()), (Vec::new(), Vec::new()));
        }

        // An unfinished IME composition suppresses both field clipboard actions.
        app.windows.get_mut(&owner).unwrap().ime.handle_preedit("ni", None);
        app.__test_set_memory_clipboard("blocked");
        assert!(app.run_action_for_window(&Action::PasteFromClipboard, owner));
        assert!(app.run_action_for_window(&Action::CopyToClipboard, owner));
        app.windows.get_mut(&owner).unwrap().ime.cancel();
        assert_eq!(app.command_palette.query(), "你好");
        assert_eq!(app.__test_memory_clipboard().as_deref(), Some("blocked"));
        assert_eq!((app.__test_drain_pty_writes(), submitted.take()), (Vec::new(), Vec::new()));

        // A window that does not host the palette keeps its ordinary terminal paste.
        app.__test_set_memory_clipboard("u");
        app.wait_for_input_queues();
        assert!(app.run_action_for_window(&Action::PasteFromClipboard, unrelated));
        let expected = vec![(unrelated_pane, b"u".to_vec())];
        assert_eq!((app.__test_drain_pty_writes(), submitted.take()), (expected.clone(), expected));
        assert_eq!(app.command_palette.query(), "你好");

        // A lost tab capture or a hidden owner cannot retarget the paste to a terminal.
        app.tab_edit_target = None;
        assert!(app.run_action_for_window(&Action::PasteFromClipboard, owner));
        assert_eq!(app.command_palette.query(), "你好");
        open_field_palette(&mut app, owner, CommandPaletteMode::Commands);
        app.windows.get_mut(&owner).unwrap().hidden = true;
        assert!(app.run_action_for_window(&Action::PasteFromClipboard, owner));
        app.windows.get_mut(&owner).unwrap().hidden = false;
        assert_eq!(app.command_palette.query(), "");
        app.wait_for_input_queues();
        assert_eq!((app.__test_drain_pty_writes(), submitted.take()), (Vec::new(), Vec::new()));

        // The color picker is modal and not editable: both actions are consumed without effect.
        close_field_palette(&mut app);
        assert!(app.run_action_for_window(&Action::OpenCommandPalette, owner));
        assert!(app.run_action_for_window(&Action::UpdateTabColor, owner));
        assert_eq!(app.command_palette.mode(), CommandPaletteMode::TabColor);
        app.__test_set_memory_clipboard("sentinel");
        assert!(app.run_action_for_window(&Action::PasteFromClipboard, owner));
        assert!(app.run_action_for_window(&Action::CopyToClipboard, owner));
        assert_eq!(app.__test_memory_clipboard().as_deref(), Some("sentinel"));
        assert_eq!(app.command_palette.query(), "");
        app.wait_for_input_queues();
        assert_eq!((app.__test_drain_pty_writes(), submitted.take()), (Vec::new(), Vec::new()));
        close_field_palette(&mut app);
    }
}

/// Search owns copy of its own selection; without one, copy never falls back to the terminal selection.
#[cfg(any(windows, unix))]
#[test]
fn real_pty_search_field_copy_uses_query_selection_only() {
    if isolated() {
        return;
    }
    for source_index in 0..3 {
        let (mut app, windows) = search_paste_fixture();
        let (source_window, source) = windows[source_index];
        let (other_window, _) = windows[(source_index + 1) % 3];
        seed_terminal_selection(&mut app, source_window, source);
        app.frontmost_window = Some(other_window);
        set_search_paste_query(&mut app, source_window, "你é好");
        app.__test_set_memory_clipboard("sentinel");
        // No query selection: the terminal selection must stay out of the clipboard.
        assert!(app.run_action_for_window(&Action::CopyToClipboard, source_window));
        assert_eq!(app.__test_memory_clipboard().as_deref(), Some("sentinel"));
        let window = app.windows.get_mut(&source_window).unwrap();
        let search = window.tab_states[0].search.as_mut().unwrap();
        search.set_cursor("你".len());
        search.extend_to("你é".len());
        let (matches, current) = (search.matches.len(), search.current);
        // Copy needs no active pane or grid lock, and READONLY still permits it.
        let removed = window.panes.remove(&source).unwrap();
        window.copy_mode = Some(CopyModeState::read_only_at((0, 0)));
        assert!(app.run_action_for_window(&Action::CopyToClipboard, source_window));
        app.windows.get_mut(&source_window).unwrap().panes.insert(source, removed);
        assert_eq!(app.__test_memory_clipboard().as_deref(), Some("é"));
        let search = app.windows[&source_window].tab_states[0].search.as_ref().unwrap();
        assert_eq!(search.selected_text(), Some("é"));
        assert_eq!((search.matches.len(), search.current), (matches, current));
        assert!(app.windows[&source_window].selection.is_some());
        assert_eq!(app.__test_drain_pty_writes(), Vec::new());
    }
}

#[test]
fn keyboard_quit_confirmation_stays_on_its_source_window() {
    // The warning follows the key's owner without retargeting focus or allowing repeats to confirm quit.
    for source_is_child in [false, true] {
        let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
        app.__test_seed_tab("main");
        let main = app.main_window_id.unwrap();
        let child = app.__test_seed_child_window(&["child"]);
        let (source, other) = if source_is_child { (child, main) } else { (main, child) };
        app.frontmost_window = Some(other);
        app.__test_enable_pty_write_log();

        assert!(app.on_quit_chord_pressed(source, false));
        assert_eq!(
            app.windows[&source].notification.as_ref().map(|bubble| bubble.message.as_str()),
            Some(super::super::super::super::quit_hold::QUIT_CONFIRM_PROMPT)
        );
        assert!(app.windows[&other].notification.is_none());
        assert_eq!(app.frontmost_window, Some(other));
        assert!(!app.pending_exit);
        assert!(app.on_quit_chord_pressed(source, true));
        assert!(!app.pending_exit);
        assert!(app.on_quit_chord_pressed(source, false));
        assert!(app.pending_exit);
        assert!(app.__test_drain_pty_writes().is_empty());
    }
}

#[test]
fn stale_keyboard_quit_cannot_arm_a_live_window() {
    // A removed source must not contribute the first press or place a warning on the frontmost window.
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    let main = app.main_window_id.unwrap();
    let child = app.__test_seed_child_window(&["removed"]);
    app.windows.remove(&child);
    app.frontmost_window = Some(main);

    assert!(!app.on_quit_chord_pressed(child, false));
    assert!(app.windows[&main].notification.is_none());
    assert!(!app.pending_exit);
    assert!(app.on_quit_chord_pressed(main, false));
    assert!(!app.pending_exit);
    assert!(app.windows[&main].notification.is_some());
}
