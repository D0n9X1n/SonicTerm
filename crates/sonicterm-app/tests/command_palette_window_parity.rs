//! Command palette routing parity tests.
//!
//! The visual compactness of the palette is computed in `sonicterm-ui`, but
//! the app decides which window receives the shared palette overlay. These
//! tests pin that the same compact palette can attach to either the main window
//! or a torn-out child window, so main/child cannot drift.

use sonicterm_app::app::{config_diff_needs_font_apply, os_drag::DragOutcome, App};
use sonicterm_cfg::{
    config::Config,
    keymap::{Action, Keymap},
    theme::Theme,
};
use winit::{
    event::Ime,
    keyboard::{Key, ModifiersState},
};

fn app() -> App {
    App::new(Theme::default(), Config::default(), Keymap::default())
}

#[test]
fn command_palette_opens_attached_to_main_when_main_is_frontmost() {
    let mut app = app();
    app.__test_seed_tab("main");
    // In headless tests there is no real winit main window, so leave
    // frontmost unset; production treats `None` as the safe main-window
    // fallback and attaches the palette to main.
    assert!(app.run_action(&Action::OpenCommandPalette));

    assert!(app.__test_palette_open());
    assert_eq!(
        app.__test_palette_attached_window(),
        None,
        "main-frontmost palette should render on main (None attachment)"
    );
}

#[test]
fn command_palette_opens_attached_to_child_when_child_is_frontmost() {
    let mut app = app();
    app.__test_seed_tab("main");
    let child = app.__test_seed_child_window(&["child"]);
    app.__test_set_frontmost_window(Some(child));

    assert!(app.run_action(&Action::OpenCommandPalette));

    assert!(app.__test_palette_open());
    assert_eq!(
        app.__test_palette_attached_window(),
        Some(child),
        "child-frontmost palette should render on that child window"
    );
}

#[test]
fn command_palette_accepts_ime_commit_text_on_main_attachment() {
    let mut app = app();
    app.__test_seed_tab("main");
    assert!(app.run_action(&Action::OpenCommandPalette));

    assert!(app.__test_command_palette_handle_ime(&Ime::Commit("重命名".into())));

    assert!(app.__test_input_dirty());
    assert_eq!(app.__test_palette_query(), "重命名");
    assert_eq!(app.__test_palette_cursor(), "重命名".len());
    assert_eq!(
        app.__test_main_ime_candidate_anchor_kind(),
        "palette",
        "main palette IME candidates must anchor to the palette input caret"
    );
}

#[test]
fn command_palette_ime_preedit_marks_input_dirty() {
    let mut app = app();
    app.__test_seed_tab("main");
    assert!(app.run_action(&Action::OpenCommandPalette));

    assert!(app.__test_command_palette_handle_ime(&Ime::Preedit("zhong".into(), Some((5, 5)))));

    assert!(app.__test_input_dirty());
}

#[test]
fn command_palette_core_edits_apply_on_main_and_child_attachments() {
    for child_attached in [false, true] {
        let mut app = app();
        app.__test_seed_tab("main");
        if child_attached {
            let child = app.__test_seed_child_window(&["child"]);
            app.__test_set_frontmost_window(Some(child));
        }
        assert!(app.run_action(&Action::OpenCommandPalette));
        assert!(app.__test_command_palette_handle_ime(&Ime::Commit("alpha beta".into())));

        assert!(app.__test_command_palette_text_edit(
            &Key::Character("w".into()),
            ModifiersState::CONTROL,
        ));

        assert_eq!(app.__test_palette_query(), "alpha ");
        assert_eq!(app.__test_palette_cursor(), "alpha ".len());
    }
}

#[test]
fn rename_mode_uses_the_same_core_editing_logic() {
    let mut app = app();
    app.__test_seed_tab("main");
    assert!(app.run_action(&Action::OpenCommandPalette));
    app.__test_start_rename_tab("alpha🙂omega");

    assert!(
        app.__test_command_palette_text_edit(&Key::Character("a".into()), ModifiersState::CONTROL,)
    );
    assert!(
        app.__test_command_palette_text_edit(&Key::Character("f".into()), ModifiersState::CONTROL,)
    );
    assert!(
        app.__test_command_palette_text_edit(&Key::Character("k".into()), ModifiersState::CONTROL,)
    );

    assert_eq!(app.__test_palette_query(), "a");
    assert_eq!(app.__test_palette_cursor(), 1);
}

#[test]
fn active_palette_ime_preedit_suppresses_core_editing() {
    let mut app = app();
    app.__test_seed_tab("main");
    assert!(app.run_action(&Action::OpenCommandPalette));
    assert!(app.__test_command_palette_handle_ime(&Ime::Commit("alpha beta".into())));
    assert!(app.__test_command_palette_handle_ime(&Ime::Preedit("ni".into(), Some((2, 2)))));

    assert!(
        app.__test_command_palette_text_edit(&Key::Character("w".into()), ModifiersState::CONTROL,)
    );

    assert_eq!(app.__test_palette_query(), "alpha beta");
}

#[test]
fn weight_scale_only_change_reapplies_font_rendering() {
    let old = Config::default();
    let mut new = old.clone();
    new.font.weight_scale = 1.1;
    assert!(config_diff_needs_font_apply(&old, &new));
}

#[test]
fn move_tab_to_new_window_queues_active_main_tab_without_detaching() {
    let mut app = app();
    app.__test_seed_tab("first");
    app.__test_seed_tab("second");
    let main = app.__test_main_window_id().expect("synthetic main");

    assert!(app.run_action_for_window(&Action::MoveTabToNewWindow, main));

    assert_eq!(app.__test_pending_tear_out(), Some((main, 1, None)));
    assert_eq!(app.__test_main_tab_count(), 2, "detach waits for native-window creation");
}

#[test]
fn move_tab_to_new_window_queues_active_child_tab_without_detaching() {
    let mut app = app();
    app.__test_seed_tab("main");
    let child = app.__test_seed_child_window(&["first", "second"]);
    assert!(app.__test_invoke_activate_tab_in_child(child, 0));

    assert!(app.run_action_for_window(&Action::MoveTabToNewWindow, child));

    assert_eq!(app.__test_pending_tear_out(), Some((child, 0, None)));
    assert_eq!(app.__test_child_tab_count(child), Some(2));
}

#[test]
fn drag_tear_out_keeps_its_screen_position() {
    let mut app = app();
    app.__test_seed_tab("main");
    let main = app.__test_main_window_id().expect("synthetic main");
    app.__test_set_os_drag_source(Some((main, 0)));
    app.__test_os_drag_pending()
        .set_ended(DragOutcome::DroppedOnEmpty { drop_screen_pos: (640, 360) });

    assert_eq!(
        app.handle_os_drag_ended(),
        Some(DragOutcome::DroppedOnEmpty { drop_screen_pos: (640, 360) })
    );
    assert_eq!(app.__test_pending_tear_out(), Some((main, 0, Some((640, 360)))));
}

#[test]
fn command_palette_accepts_ime_commit_text_on_child_attachment() {
    let mut app = app();
    app.__test_seed_tab("main");
    let child = app.__test_seed_child_window(&["child"]);
    app.__test_set_frontmost_window(Some(child));
    assert!(app.run_action(&Action::OpenCommandPalette));

    assert!(app.__test_command_palette_handle_ime(&Ime::Commit("设置".into())));

    assert!(app.__test_input_dirty());
    assert_eq!(app.__test_palette_attached_window(), Some(child));
    assert_eq!(app.__test_palette_query(), "设置");
    assert_eq!(
        app.__test_child_ime_candidate_anchor_kind(child),
        Some("palette"),
        "child palette IME candidates must anchor to the palette input caret"
    );
}
