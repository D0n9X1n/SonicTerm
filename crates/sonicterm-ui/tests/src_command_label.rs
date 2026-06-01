//! Tests for the `command_label` helpers (action labels, key prettifier).
//!
//! Migrated from inline `#[cfg(test)] mod tests` in `src/command_label.rs`.

use sonicterm_cfg::keymap::{Action, BroadcastScope, Direction, ScrollAction};
use sonicterm_ui::command_label::{label, pretty_keys, variant_kind, ALL_VARIANT_KINDS};

#[test]
fn variant_kind_covers_every_action() {
    let samples: Vec<Action> = vec![
        Action::NewTab,
        Action::CloseTab,
        Action::CloseActivePaneOrTab,
        Action::NextTab,
        Action::PrevTab,
        Action::ActivateTab(1),
        Action::ActivateLastTab,
        Action::SplitRight,
        Action::SplitDown,
        Action::ClosePane,
        Action::TogglePaneZoom,
        Action::ToggleBroadcast { scope: BroadcastScope::Tab },
        Action::FocusPane(Direction::Left),
        Action::ResizePaneLeft,
        Action::ResizePaneRight,
        Action::ResizePaneUp,
        Action::ResizePaneDown,
        Action::ResizePane { dir: Direction::Left, amount: 1 },
        Action::CopyToClipboard,
        Action::EnterCopyMode,
        Action::PasteFromClipboard,
        Action::IncreaseFontSize,
        Action::DecreaseFontSize,
        Action::ResetFontSize,
        Action::ApplyTheme("dracula".into()),
        Action::ToggleTabBar,
        Action::NewWindow,
        Action::ToggleFullscreen,
        Action::OpenSearch,
        Action::OpenCommandPalette,
        Action::ShowKeymapCheatsheet,
        Action::EditConfigFile,
        Action::OpenKeymapFile,
        Action::Scroll(ScrollAction::PageUp),
        Action::ScrollToPrevPrompt,
        Action::ScrollToNextPrompt,
        Action::ReloadConfig,
        Action::OpenSshPane("user@host".into()),
    ];
    assert_eq!(samples.len(), ALL_VARIANT_KINDS.len(), "samples must cover every variant kind");
    for s in &samples {
        let k = variant_kind(s);
        assert!(
            ALL_VARIANT_KINDS.contains(&k),
            "variant_kind({s:?}) = {k} not in ALL_VARIANT_KINDS"
        );
    }
}

#[test]
fn labels_are_human_readable_verb_noun() {
    assert_eq!(label(&Action::NewTab), "New Tab");
    assert_eq!(label(&Action::SplitRight), "Split Pane Right");
    assert_eq!(label(&Action::EditConfigFile), "Edit sonic.toml");
    assert_eq!(label(&Action::ReloadConfig), "Reload Config");
}

#[test]
fn pretty_keys_translates_modifiers() {
    assert_eq!(pretty_keys("super+t"), "⌘T");
    assert_eq!(pretty_keys("super+shift+p"), "⌘⇧P");
    assert_eq!(pretty_keys("ctrl+alt+enter"), "⌃⌥Enter");
}
