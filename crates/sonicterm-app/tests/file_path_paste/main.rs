use std::path::PathBuf;

use sonicterm_app::app::{App, FrontmostKind};
use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};

fn app() -> App {
    App::new(Theme::default(), Config::default(), Keymap::default())
}

#[test]
fn dropped_file_paths_paste_shell_escaped_text_to_main_pane() {
    let mut app = app();
    let pane = app.__test_seed_tab("main");

    app.__test_paste_file_paths_for_kind(
        FrontmostKind::Main,
        vec![PathBuf::from("/tmp/video file.mp4"), PathBuf::from("/tmp/it's.txt")],
    );

    let writes = app.__test_drain_pty_writes();
    assert_eq!(writes, vec![(pane, b"'/tmp/video file.mp4' '/tmp/it'\\''s.txt'".to_vec())]);
}

#[test]
fn dropped_file_paths_target_frontmost_child_pane() {
    let mut app = app();
    app.__test_seed_tab("main");
    let child = app.__test_seed_child_window(&["child"]);
    let pane = app.__test_child_active_pane(child).expect("child has active pane");

    app.__test_paste_file_paths_for_kind(
        FrontmostKind::Child(child),
        vec![PathBuf::from("C:/Users/me/movie.mkv")],
    );

    let writes = app.__test_drain_pty_writes();
    assert_eq!(writes, vec![(pane, b"'C:/Users/me/movie.mkv'".to_vec())]);
}
