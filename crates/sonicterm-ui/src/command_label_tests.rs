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

/// Key hints normalize known modifiers and preserve unknown tokens visibly.
#[test]
fn pretty_keys_normalizes_without_dropping_unknown_tokens() {
    assert_eq!(pretty_keys("super+shift+p"), "⌘⇧P");
    assert_eq!(pretty_keys("ctrl+alt+left"), "⌃⌥←");
    assert_eq!(pretty_keys("hyper+space"), "HyperSpace");
}
