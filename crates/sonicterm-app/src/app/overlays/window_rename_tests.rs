use super::App;
use sonicterm_cfg::config::Config;
use sonicterm_cfg::keymap::{Action, Keymap};
use sonicterm_cfg::theme::Theme;
use winit::keyboard::{Key, NamedKey};

#[test]
fn window_name_paste_is_atomic_and_never_reaches_broadcast() {
    // Paste through the explicit-source action must reject malformed chunks without forwarding even one byte to panes.
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    let main = app.main_window_id.unwrap();
    app.start_rename_window(main);
    app.__test_set_memory_clipboard("工作");
    assert!(app.run_action_for_window(&Action::PasteFromClipboard, main));
    assert_eq!(app.command_palette.query(), "工作");
    app.__test_set_memory_clipboard("bad\ninput");
    assert!(app.run_action_for_window(&Action::PasteFromClipboard, main));
    assert_eq!(app.command_palette.query(), "工作");
    app.command_palette_handle_logical_key(&Key::Named(NamedKey::Enter));
    assert!(app.command_palette.is_open());
    assert_eq!(app.native_window_title(main).as_deref(), Some("#1 SonicTerm"));
    assert!(app.__test_drain_pty_writes().is_empty());
}
