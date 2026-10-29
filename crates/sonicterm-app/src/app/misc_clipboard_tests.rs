use super::*;

/// Copied files paste as quoted paths through the drop encoder, and win over the text a file copy also carries.
#[test]
fn clipboard_files_paste_as_quoted_paths() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let pane = app.__test_seed_tab("main");
    // Finder also publishes the bare file name as text; the path list must take precedence.
    app.test_clipboard_text = Some("report one.txt".into());
    app.test_clipboard_files = Some(vec!["/tmp/report one.txt".into(), "/tmp/it's here".into()]);
    app.__test_enable_pty_write_log();
    app.paste_clipboard_for_kind(FrontmostKind::Main);
    assert_eq!(
        app.__test_drain_pty_writes(),
        [(pane, b"'/tmp/report one.txt' '/tmp/it'\\''s here'".to_vec())]
    );
}

/// A clipboard holding only text keeps the existing text paste unchanged.
#[test]
fn clipboard_text_without_files_pastes_text() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let pane = app.__test_seed_tab("main");
    app.test_clipboard_text = Some("plain text".into());
    app.test_clipboard_files = Some(Vec::new());
    app.__test_enable_pty_write_log();
    app.paste_clipboard_for_kind(FrontmostKind::Main);
    assert_eq!(app.__test_drain_pty_writes(), [(pane, b"plain text".to_vec())]);
}

/// Copied files use the drop path's refusals: one unsafe path refuses the whole list and nothing reaches the shell.
#[test]
fn clipboard_files_reuse_drop_refusals() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    app.test_clipboard_files = Some(vec!["/tmp/ok".into(), "/tmp/bad\nname".into()]);
    app.__test_enable_pty_write_log();
    app.paste_clipboard_for_kind(FrontmostKind::Main);
    assert!(app.__test_drain_pty_writes().is_empty());
    let notice = app.main().unwrap().notification.as_ref().unwrap().message.clone();
    assert!(notice.contains("ControlCharacter"), "{notice}");
}

/// An open search owns clipboard paste and stays text-only, so copied files never reach its query or the shell.
#[test]
fn clipboard_files_do_not_reach_open_search() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    app.__test_seed_tab("main");
    app.open_search();
    app.test_clipboard_text = Some("needle".into());
    app.test_clipboard_files = Some(vec!["/tmp/file".into()]);
    app.__test_enable_pty_write_log();
    app.paste_clipboard_for_kind(FrontmostKind::Main);
    assert!(app.__test_drain_pty_writes().is_empty());
    let search = app.main().unwrap().tab_states[0].search.as_ref().unwrap();
    assert_eq!(search.query, "needle");
}

/// Clipboard file paths stay attributed to `Paste`, while a native drop of the same paths stays `FileDrop`.
#[test]
fn clipboard_files_keep_paste_source_attribution() {
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let pane = app.__test_seed_tab("main");
    app.test_clipboard_files = Some(vec!["/tmp/file".into()]);
    app.__test_enable_pty_write_log();
    app.paste_clipboard_for_kind(FrontmostKind::Main);
    app.paste_file_paths_for_kind(FrontmostKind::Main, vec!["/tmp/file".into()]);
    assert_eq!(
        *app.test_pty_write_sources.borrow(),
        [(pane, crate::app::PtyInputSource::Paste), (pane, crate::app::PtyInputSource::FileDrop)]
    );
}
