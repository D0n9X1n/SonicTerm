use super::*;
use crate::command_palette::all_actions;
use std::collections::BTreeSet;

/// Variant identifiers are unique and cover every canonical representative action.
#[test]
fn variant_kinds_are_unique_and_exhaustive() {
    let declared: BTreeSet<&str> = ALL_VARIANT_KINDS.iter().copied().collect();
    assert_eq!(declared.len(), ALL_VARIANT_KINDS.len());
    let represented: BTreeSet<&str> = all_actions().iter().map(variant_kind).collect();
    assert_eq!(represented, declared);
}

/// Metadata retains existing action identities, parameter-independent keys, and search aliases.
#[test]
fn command_descriptors_preserve_existing_identity_and_aliases() {
    for action in all_actions() {
        let entry = descriptor(&action);
        assert_eq!(entry.id, variant_kind(&action));
        assert!(ALL_VARIANT_KINDS.contains(&entry.id));
        assert_eq!(entry.aliases, keywords(&action));
        assert!(entry.label_key.starts_with("command-"));
    }
    assert_eq!(descriptor(&Action::ActivateTab(0)), descriptor(&Action::ActivateTab(19)));
    assert_eq!(
        descriptor(&Action::ApplyTheme("one".into())),
        descriptor(&Action::ApplyTheme("two".into()))
    );
    assert_eq!(
        descriptor(&Action::CloseActivePaneOrTab).aliases,
        &["close", "quit", "x", "pane", "tab", "cmd+w"]
    );
    assert_eq!(
        descriptor(&Action::QuitApp).aliases,
        &["quit", "exit", "close app", "cmd+q", "terminate"]
    );
}

/// Requirements use the attached window's facts without granting actions outside the READONLY whitelist.
#[test]
fn command_context_requirements_and_readonly_policy_are_explicit() {
    let mut context = CommandContext::default();
    assert_eq!(disabled_reason(&Action::NewTab, &context), Some(DisabledReason::NoWindow));
    assert_eq!(disabled_reason(&Action::NewWindow, &context), None);
    context.window_available = true;
    assert_eq!(disabled_reason(&Action::CloseTab, &context), Some(DisabledReason::NoTab));
    context.tab_count = 2;
    assert_eq!(disabled_reason(&Action::SplitRight, &context), Some(DisabledReason::NoPane));
    context.pane_available = true;
    assert_eq!(
        disabled_reason(&Action::CopyToClipboard, &context),
        Some(DisabledReason::NoSelection)
    );
    context.selection_available = true;
    assert_eq!(disabled_reason(&Action::CopyToClipboard, &context), None);
    assert_eq!(disabled_reason(&Action::ActivateTab(1), &context), None);
    assert_eq!(
        disabled_reason(&Action::ActivateTab(2), &context),
        Some(DisabledReason::MissingTab)
    );
    for (index, direction) in
        [Direction::Left, Direction::Right, Direction::Up, Direction::Down].into_iter().enumerate()
    {
        assert_eq!(
            disabled_reason(&Action::FocusPane(direction), &context),
            Some(DisabledReason::NoNeighbor)
        );
        context.focus_available[index] = true;
        assert_eq!(disabled_reason(&Action::FocusPane(direction), &context), None);
    }
    context.read_only = true;
    for action in all_actions() {
        let expected = matches!(
            action,
            Action::NextTab
                | Action::PrevTab
                | Action::ActivateTab(_)
                | Action::ActivateLastTab
                | Action::FocusPane(_)
                | Action::OpenSearch
                | Action::CheckForUpdates
                | Action::SaveCurrentSettings
                | Action::OpenCommandPalette
                | Action::RenameWindow
        );
        assert_eq!(descriptor(&action).read_only_allowed, expected, "{action:?}");
        assert_eq!(
            disabled_reason(&action, &context),
            (!expected).then_some(DisabledReason::ReadOnly),
            "{action:?}"
        );
    }
}

/// Every action has a visible label and a haystack containing that label and its aliases.
#[test]
fn labels_and_keywords_feed_one_search_haystack() {
    for action in all_actions() {
        let label = label(&action);
        let haystack = search_haystack(&action);
        assert!(!label.is_empty(), "{action:?} has no display label");
        assert!(haystack.starts_with(&label), "{action:?} haystack lost its label");
        for keyword in keywords(&action) {
            assert!(haystack.contains(keyword), "{action:?} haystack lost keyword {keyword:?}");
        }
    }
}

/// Parameterized actions keep their values in human-readable labels.
#[test]
fn parameterized_labels_preserve_action_values() {
    assert_eq!(label(&Action::ActivateTab(2)), "Activate Tab 3");
    assert_eq!(
        label(&Action::ResizePane { dir: Direction::Left, amount: 7 }),
        "Resize Pane Left by 7"
    );
    assert_eq!(label(&Action::ApplyTheme("gruvbox".into())), "Apply Theme: gruvbox");
}

/// Native hint formatting changes presentation, never the keymap chord or its token order.
#[test]
fn pretty_keys_uses_native_modifier_names() {
    let expected = if cfg!(target_os = "macos") {
        "⌘⇧P"
    } else if cfg!(target_os = "windows") {
        "Win+Shift+P"
    } else {
        "Super+Shift+P"
    };
    assert_eq!(pretty_keys("super+shift+p"), expected);
    let expected = if cfg!(target_os = "macos") { "⌃⌥←" } else { "Ctrl+Alt+Left" };
    assert_eq!(pretty_keys("ctrl+alt+left"), expected);
}

/// All hosts pin every platform spelling, aliases, and literal-plus handling independently.
#[test]
fn platform_shortcut_spellings_are_explicit() {
    use ShortcutPlatform::{Linux, Mac, Windows};
    for (platform, super_hint, control_hint, plus_hint) in [
        (Mac, "⌘⇧P", "⌃⌥←", "⌘+"),
        (Windows, "Win+Shift+P", "Ctrl+Alt+Left", "Win++"),
        (Linux, "Super+Shift+P", "Ctrl+Alt+Left", "Super++"),
    ] {
        assert_eq!(pretty_keys_for_platform("super+shift+p", platform), super_hint);
        assert_eq!(pretty_keys_for_platform("control+option+left", platform), control_hint);
        assert_eq!(pretty_keys_for_platform("super++", platform), plus_hint);
        assert_eq!(pretty_keys_for_platform("+", platform), "+");
        assert_eq!(pretty_keys_for_platform("", platform), "");
    }
    assert_eq!(pretty_keys_for_platform("hyper+space", Mac), "HyperSpace");
    assert_eq!(pretty_keys_for_platform("hyper+space", Windows), "Hyper+Space");
    assert_eq!(pretty_keys_for_platform("ctrl+pageup", Linux), "Ctrl+PageUp");
}

/// Literal plus keys and unfamiliar tokens remain visible instead of disappearing into separators.
#[test]
fn pretty_keys_preserves_plus_and_unknown_tokens() {
    let expected = if cfg!(target_os = "macos") { "⌥+" } else { "Alt++" };
    assert_eq!(pretty_keys("alt++"), expected);
    assert_eq!(pretty_keys("+"), "+");
    assert_eq!(pretty_keys(""), "");
    let expected = if cfg!(target_os = "macos") { "HyperSpace" } else { "Hyper+Space" };
    assert_eq!(pretty_keys("hyper+space"), expected);
}
