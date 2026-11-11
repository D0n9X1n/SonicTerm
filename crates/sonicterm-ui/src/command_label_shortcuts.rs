//! Keyboard shortcut hints for palette commands: the first chord bound to an action, written
//! with each platform's modifier names.

use sonicterm_cfg::keymap::{Action, Keymap};

/// Look up the first keybinding bound to `action` in the keymap.
/// Returns `None` for actions that aren't bound, which the palette
/// renders as no hint (the user can still trigger them by name).
#[must_use]
pub fn keybinding_hint(keymap: &Keymap, action: &Action) -> Option<String> {
    keymap
        .bindings
        .iter()
        .find(|binding| &binding.action.0 == action)
        .map(|binding| pretty_keys(&binding.keys))
}

/// The platform whose modifier names a shortcut hint uses.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ShortcutPlatform {
    Mac,
    Windows,
    Linux,
}

/// Display a keymap chord using native modifier names without changing its binding.
#[doc(hidden)]
pub fn pretty_keys(raw: &str) -> String {
    let platform = if cfg!(target_os = "macos") {
        // When: `target_os` is `macos`, retain native modifier glyphs in the hint.
        ShortcutPlatform::Mac
    } else if cfg!(target_os = "windows") {
        // When: `target_os` is `windows`, name the Super modifier after the Win key.
        ShortcutPlatform::Windows
    } else {
        // When: `target_os` is neither `macos` nor `windows`, use the Linux textual convention.
        ShortcutPlatform::Linux
    };
    pretty_keys_for_platform(raw, platform)
}

/// [`pretty_keys`] for an explicit `platform`, so every platform's names are testable on any host.
fn pretty_keys_for_platform(raw: &str, platform: ShortcutPlatform) -> String {
    let mac = platform == ShortcutPlatform::Mac;
    let mut tokens: Vec<&str> = raw.split('+').collect();
    if raw.ends_with('+') {
        // A literal `+` key produces empty split fields but must remain visible in the hint.
        tokens.pop();
        if tokens.last() == Some(&"") {
            tokens.pop();
        }
        tokens.push("+");
    }
    tokens
        .into_iter()
        .map(|token| {
            let lower = token.to_ascii_lowercase();
            let known = match lower.as_str() {
                "super" | "cmd" | "command" => match platform {
                    ShortcutPlatform::Mac => Some("⌘"),
                    ShortcutPlatform::Windows => Some("Win"),
                    ShortcutPlatform::Linux => Some("Super"),
                },
                "shift" => Some(if mac { "⇧" } else { "Shift" }),
                "ctrl" | "control" => Some(if mac { "⌃" } else { "Ctrl" }),
                "alt" | "option" | "opt" => Some(if mac { "⌥" } else { "Alt" }),
                "left" => Some(if mac { "←" } else { "Left" }),
                "right" => Some(if mac { "→" } else { "Right" }),
                "up" => Some(if mac { "↑" } else { "Up" }),
                "down" => Some(if mac { "↓" } else { "Down" }),
                "enter" | "return" => Some(if mac { "↵" } else { "Enter" }),
                "esc" | "escape" => Some("Esc"),
                "space" => Some("Space"),
                "tab" => Some("Tab"),
                "pageup" => Some("PageUp"),
                "pagedown" => Some("PageDown"),
                "home" => Some("Home"),
                "end" => Some("End"),
                _ => None,
            };
            known.map(str::to_string).unwrap_or_else(|| {
                let mut chars = lower.chars();
                match chars.next() {
                    Some(first) => first.to_ascii_uppercase().to_string() + chars.as_str(),
                    None => String::new(),
                }
            })
        })
        .collect::<Vec<_>>()
        .join(if mac { "" } else { "+" })
}

#[cfg(test)]
#[path = "command_label_shortcuts_tests.rs"]
mod command_label_shortcuts_tests;
