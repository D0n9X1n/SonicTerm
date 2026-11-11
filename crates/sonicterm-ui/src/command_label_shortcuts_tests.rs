use super::*;

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
