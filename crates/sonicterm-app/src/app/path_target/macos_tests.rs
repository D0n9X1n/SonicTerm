//! macOS-only path-target tests: trimmed-path probing from a pane directory, activation-time
//! reveal revalidation, and classification of executable files.

#![cfg(target_os = "macos")]

use super::*;
use crate::app::path_target::path_target_tests::native_test_root;
use sonicterm_cfg::{config::Config, keymap::Keymap, theme::Theme};

/// Trusted pane context selects the trimmed source span first; the punctuation literal opens only once it is gone.
#[cfg(target_os = "macos")]
#[test]
fn app_probe_prefers_trimmed_then_literal_revealable_path() {
    let root = native_test_root().join(format!(
        "sonicterm-prose-path-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    let source_dir = root.join("lua/config");
    std::fs::create_dir_all(&source_dir).unwrap();
    let trimmed_path = source_dir.join("lsp.lua");
    let literal_path = source_dir.join("lsp.lua,");
    std::fs::write(&trimmed_path, b"return {}").unwrap();
    std::fs::write(&literal_path, b"punctuation filename").unwrap();

    let text = "lua/config/lsp.lua,";
    let mut app = App::new(Theme::default(), Config::default(), Keymap::default());
    let window = app.__test_seed_child_window(&["prose"]);
    let pane = app.__test_child_pane_ids(window).unwrap()[0];
    let output = format!("\x1b]7;file://{}\x1b\\{text}", root.display());
    assert!(app.__test_advance_child_pane_parser(window, pane, output.as_bytes()));
    let snapshot = app.cell_target_at(window, pane, 0, 2).expect("relative path candidate set");
    let ResolvedCellTarget::Path(key) = snapshot.target else {
        panic!("relative path must require a filesystem probe")
    };

    let trimmed = select_openable_candidate(&key.candidates, classify_local_target)
        .expect("the shorter existing source file wins");
    assert_eq!(trimmed.candidate.display(), "lua/config/lsp.lua");
    assert_eq!(trimmed.candidate.spans[0].end_col, u16::try_from(text.len() - 1).unwrap());
    assert_eq!(trimmed.decision, PathOpenDecision::Openable(PathKind::File));
    assert_eq!(local_target_action(trimmed.decision), Some(LocalTargetAction::Reveal));

    std::fs::remove_file(&trimmed_path).unwrap();
    let literal = select_openable_candidate(&key.candidates, classify_local_target)
        .expect("the punctuation filename opens once the shorter path is gone");
    assert_eq!(literal.candidate.display(), text);
    assert_eq!(literal.candidate.spans[0].end_col, u16::try_from(text.len()).unwrap());
    assert_eq!(literal.decision, PathOpenDecision::Openable(PathKind::File));

    std::fs::remove_dir_all(root).unwrap();
}

/// Activation-time macOS revalidation requires the exact reveal action and kind to remain stable.
#[cfg(target_os = "macos")]
#[test]
fn macos_reveal_revalidation_preserves_selection_after_mode_change() {
    use std::os::unix::fs::PermissionsExt;

    let path = native_test_root().join(format!(
        "sonicterm-reveal-revalidate-{}-{}-source.lua",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::write(&path, b"ordinary").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let decision = PathOpenDecision::Openable(PathKind::File);
    let spec =
        macos_validated_open_spec(&path, decision).expect("stable source remains revealable");
    assert_eq!(spec.args[0], "-R");

    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(macos_validated_open_spec(&path, decision).unwrap().args[0], "-R");
    std::fs::remove_file(path).unwrap();
}

/// macOS executable mode never changes file selection into execution.
#[cfg(target_os = "macos")]
#[test]
fn macos_classification_reveals_executable_file() {
    use std::os::unix::fs::PermissionsExt;

    let path = native_test_root().join(format!(
        "sonicterm-macos-executable-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::write(&path, b"ordinary").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();

    assert_eq!(classify_macos_target(&path), PathOpenDecision::Openable(PathKind::File));
    std::fs::remove_file(path).unwrap();
}
