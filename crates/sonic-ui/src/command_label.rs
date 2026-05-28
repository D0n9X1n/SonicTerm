//! Human-readable labels and keybinding hints for every
//! [`sonic_cfg::keymap::Action`] variant.
//!
//! Used by the command palette to render entries in a familiar
//! "Verb Noun" style (e.g. "New Tab", "Split Pane Right") instead of
//! the raw `PascalCase` variant names. The labels are also the fuzzy-
//! match haystack — typing "newtab" or "new t" or "n t" should all
//! land on `Action::NewTab`.
//!
//! Adding a new bindable action is a four-step process:
//!
//! 1. Add a variant to [`sonic_cfg::keymap::Action`].
//! 2. Add a match arm in [`label`] returning a `&'static str` or a
//!    formatted `String`.
//! 3. Add a discriminant entry in [`ALL_VARIANT_KINDS`] so the palette
//!    universe enumeration stays exhaustive.
//! 4. Add a dispatch arm in `sonic_app::app::App::run_action`.
//!
//! The compile-time `match` in [`label`] guarantees we cannot forget
//! step 2 — the build breaks until every variant has a label. Step 3
//! is covered by the `palette_lists_every_action_variant` test.

use sonic_cfg::keymap::{Action, Direction, Keymap, ScrollAction};

/// Stable identifier for each Action variant kind. Used to enumerate
/// the universe of palette commands and to assert exhaustiveness.
///
/// `&'static str` instead of a separate enum keeps the variant list
/// human-greppable and avoids a parallel type that has to be kept in
/// sync with [`Action`] by hand.
pub const ALL_VARIANT_KINDS: &[&str] = &[
    "NewTab",
    "CloseTab",
    "NextTab",
    "PrevTab",
    "ActivateTab",
    "ActivateLastTab",
    "SplitRight",
    "SplitDown",
    "ClosePane",
    "TogglePaneZoom",
    "ToggleBroadcast",
    "FocusPane",
    "ResizePaneLeft",
    "ResizePaneRight",
    "ResizePaneUp",
    "ResizePaneDown",
    "ResizePane",
    "CopyToClipboard",
    "EnterCopyMode",
    "PasteFromClipboard",
    "IncreaseFontSize",
    "DecreaseFontSize",
    "ResetFontSize",
    "ApplyTheme",
    "ToggleTabBar",
    "NewWindow",
    "ToggleFullscreen",
    "OpenSearch",
    "OpenCommandPalette",
    "ShowKeymapCheatsheet",
    "OpenPreferences",
    "Scroll",
    "ScrollToPrevPrompt",
    "ScrollToNextPrompt",
    "ReloadConfig",
    "OpenSshPane",
];

/// The discriminant string for an [`Action`], matching one of
/// [`ALL_VARIANT_KINDS`]. Used by the palette to verify exhaustive
/// coverage of the enum at test time.
#[must_use]
pub fn variant_kind(a: &Action) -> &'static str {
    match a {
        Action::NewTab => "NewTab",
        Action::CloseTab => "CloseTab",
        Action::NextTab => "NextTab",
        Action::PrevTab => "PrevTab",
        Action::ActivateTab(_) => "ActivateTab",
        Action::ActivateLastTab => "ActivateLastTab",
        Action::SplitRight => "SplitRight",
        Action::SplitDown => "SplitDown",
        Action::ClosePane => "ClosePane",
        Action::TogglePaneZoom => "TogglePaneZoom",
        Action::ToggleBroadcast { .. } => "ToggleBroadcast",
        Action::FocusPane(_) => "FocusPane",
        Action::ResizePaneLeft => "ResizePaneLeft",
        Action::ResizePaneRight => "ResizePaneRight",
        Action::ResizePaneUp => "ResizePaneUp",
        Action::ResizePaneDown => "ResizePaneDown",
        Action::ResizePane { .. } => "ResizePane",
        Action::CopyToClipboard => "CopyToClipboard",
        Action::EnterCopyMode => "EnterCopyMode",
        Action::PasteFromClipboard => "PasteFromClipboard",
        Action::IncreaseFontSize => "IncreaseFontSize",
        Action::DecreaseFontSize => "DecreaseFontSize",
        Action::ResetFontSize => "ResetFontSize",
        Action::ApplyTheme(_) => "ApplyTheme",
        Action::ToggleTabBar => "ToggleTabBar",
        Action::NewWindow => "NewWindow",
        Action::ToggleFullscreen => "ToggleFullscreen",
        Action::OpenSearch => "OpenSearch",
        Action::OpenCommandPalette => "OpenCommandPalette",
        Action::ShowKeymapCheatsheet => "ShowKeymapCheatsheet",
        Action::OpenPreferences => "OpenPreferences",
        Action::Scroll(_) => "Scroll",
        Action::ScrollToPrevPrompt => "ScrollToPrevPrompt",
        Action::ScrollToNextPrompt => "ScrollToNextPrompt",
        Action::ReloadConfig => "ReloadConfig",
        Action::OpenSshPane(_) => "OpenSshPane",
    }
}

/// Render a human-readable label for the palette. The format is
/// "Verb Noun" so fuzzy matching against a typed query like
/// "split right" or "new tab" feels natural.
#[must_use]
pub fn label(a: &Action) -> String {
    match a {
        Action::NewTab => "New Tab".into(),
        Action::CloseTab => "Close Tab".into(),
        Action::NextTab => "Next Tab".into(),
        Action::PrevTab => "Previous Tab".into(),
        Action::ActivateTab(i) => format!("Activate Tab {i}"),
        Action::ActivateLastTab => "Activate Last Tab".into(),
        Action::SplitRight => "Split Pane Right".into(),
        Action::SplitDown => "Split Pane Down".into(),
        Action::ClosePane => "Close Pane".into(),
        Action::TogglePaneZoom => "Toggle Pane Zoom".into(),
        Action::ToggleBroadcast { scope } => {
            format!("Toggle Broadcast {}", broadcast_scope_human(*scope))
        }
        Action::FocusPane(d) => format!("Focus Pane {}", dir_human(*d)),
        Action::ResizePaneLeft => "Resize Pane Left".into(),
        Action::ResizePaneRight => "Resize Pane Right".into(),
        Action::ResizePaneUp => "Resize Pane Up".into(),
        Action::ResizePaneDown => "Resize Pane Down".into(),
        Action::ResizePane { dir, amount } => {
            format!("Resize Pane {} by {amount}", dir_human(*dir))
        }
        Action::CopyToClipboard => "Copy to Clipboard".into(),
        Action::EnterCopyMode => "Enter Copy Mode".into(),
        Action::PasteFromClipboard => "Paste from Clipboard".into(),
        Action::IncreaseFontSize => "Increase Font Size".into(),
        Action::DecreaseFontSize => "Decrease Font Size".into(),
        Action::ResetFontSize => "Reset Font Size".into(),
        Action::ApplyTheme(name) => format!("Apply Theme: {name}"),
        Action::ToggleTabBar => "Toggle Tab Bar".into(),
        Action::NewWindow => "New Window".into(),
        Action::ToggleFullscreen => "Toggle Fullscreen".into(),
        Action::OpenSearch => "Open Search".into(),
        Action::OpenCommandPalette => "Open Command Palette".into(),
        Action::ShowKeymapCheatsheet => "Show Keyboard Shortcuts".into(),
        Action::OpenPreferences => "Open Preferences".into(),
        Action::Scroll(s) => format!("Scroll {}", scroll_human(*s)),
        Action::ScrollToPrevPrompt => "Scroll to Previous Prompt".into(),
        Action::ScrollToNextPrompt => "Scroll to Next Prompt".into(),
        Action::ReloadConfig => "Reload Config".into(),
        Action::OpenSshPane(t) => format!("Open SSH Pane: {t}"),
    }
}

/// Additional words that the palette fuzzy matcher should treat as part
/// of an action's haystack so users can find commands by synonym instead
/// of by the exact label wording. Returning `&'static [&'static str]`
/// keeps this allocation-free on the hot search path.
///
/// Example: typing `sett` in the palette must surface
/// [`Action::OpenPreferences`] even though its label is
/// "Open Preferences" (no `sett` subsequence). We expose
/// `["settings", "config", "options", "prefs"]` so any of those land it.
#[must_use]
pub fn keywords(a: &Action) -> &'static [&'static str] {
    match a {
        Action::NewTab => &["create", "open"],
        Action::CloseTab => &["quit", "x"],
        Action::NextTab => &["forward", "right"],
        Action::PrevTab => &["back", "left", "previous"],
        Action::ActivateTab(_) => &["switch", "go"],
        Action::ActivateLastTab => &["recent", "switch"],
        Action::SplitRight => &["pane", "vertical", "vsplit"],
        Action::SplitDown => &["pane", "horizontal", "hsplit"],
        Action::ClosePane => &["kill", "x"],
        Action::TogglePaneZoom => &["pane", "maximize", "unzoom"],
        Action::ToggleBroadcast { .. } => {
            &["broadcast", "input", "mirror", "all panes", "all tabs"]
        }
        Action::FocusPane(_) => &["move", "switch", "navigate"],
        Action::ResizePaneLeft
        | Action::ResizePaneRight
        | Action::ResizePaneUp
        | Action::ResizePaneDown
        | Action::ResizePane { .. } => &["grow", "shrink", "nudge", "divider"],
        Action::CopyToClipboard => &["yank"],
        Action::EnterCopyMode => &["keyboard", "selection", "yank", "vim"],
        Action::PasteFromClipboard => &["yank"],
        Action::IncreaseFontSize => &["bigger", "zoom in", "larger"],
        Action::DecreaseFontSize => &["smaller", "zoom out"],
        Action::ResetFontSize => &["default", "zoom reset"],
        Action::ApplyTheme(_) => &["color", "colors", "colour", "scheme", "appearance"],
        Action::ToggleTabBar => &["hide", "show"],
        Action::NewWindow => &["create", "open"],
        Action::ToggleFullscreen => &["maximize", "full"],
        Action::OpenSearch => &["find"],
        Action::OpenCommandPalette => &["palette", "commands"],
        Action::ShowKeymapCheatsheet => &["keyboard", "shortcuts", "keys", "help", "cheatsheet"],
        // The whole point of this PR's alias path: "sett" → Open Preferences.
        Action::OpenPreferences => &["settings", "config", "options", "prefs", "preferences"],
        Action::Scroll(_) => &["page", "line", "scrollback"],
        Action::ScrollToPrevPrompt => &["jump", "prompt", "previous"],
        Action::ScrollToNextPrompt => &["jump", "prompt", "next"],
        Action::ReloadConfig => &["refresh", "config", "settings"],
        Action::OpenSshPane(_) => &["remote", "connect"],
    }
}

/// The fuzzy-search haystack for a single action: its display label plus
/// every keyword from [`keywords`], joined by spaces. Joining (rather
/// than scoring each alias separately) keeps a single nucleo score per
/// candidate which preserves the existing rank ordering behavior.
#[must_use]
pub fn search_haystack(a: &Action) -> String {
    let mut s = label(a);
    for kw in keywords(a) {
        s.push(' ');
        s.push_str(kw);
    }
    s
}

fn broadcast_scope_human(scope: sonic_cfg::keymap::BroadcastScope) -> &'static str {
    match scope {
        sonic_cfg::keymap::BroadcastScope::Tab => "Tab",
        sonic_cfg::keymap::BroadcastScope::AllTabs => "All Tabs",
    }
}

fn dir_human(d: Direction) -> &'static str {
    match d {
        Direction::Left => "Left",
        Direction::Right => "Right",
        Direction::Up => "Up",
        Direction::Down => "Down",
    }
}

fn scroll_human(s: ScrollAction) -> &'static str {
    match s {
        ScrollAction::LineUp => "Line Up",
        ScrollAction::LineDown => "Line Down",
        ScrollAction::PageUp => "Page Up",
        ScrollAction::PageDown => "Page Down",
        ScrollAction::ToTop => "To Top",
        ScrollAction::ToBottom => "To Bottom",
    }
}

/// Look up the first keybinding bound to `action` in the keymap.
/// Returns `None` for actions that aren't bound, which the palette
/// renders as no hint (the user can still trigger them by name).
#[must_use]
pub fn keybinding_hint(km: &Keymap, action: &Action) -> Option<String> {
    km.bindings.iter().find(|b| &b.action.0 == action).map(|b| pretty_keys(&b.keys))
}

/// Tidy a raw `super+shift+p` keymap string into `⌘⇧P` style for
/// display alongside the palette entry. Falls back to the raw string
/// for unknown tokens so we never lose information.
#[doc(hidden)]
pub fn pretty_keys(raw: &str) -> String {
    raw.split('+')
        .map(|tok| match tok.to_ascii_lowercase().as_str() {
            "super" | "cmd" | "command" => "⌘".to_string(),
            "shift" => "⇧".to_string(),
            "ctrl" | "control" => "⌃".to_string(),
            "alt" | "option" | "opt" => "⌥".to_string(),
            other if other.len() == 1 => other.to_ascii_uppercase(),
            other => {
                let mut c = other.chars();
                match c.next() {
                    Some(first) => first.to_ascii_uppercase().to_string() + c.as_str(),
                    None => String::new(),
                }
            }
        })
        .collect::<Vec<_>>()
        .join("")
}

// Unit tests live in `tests/src_command_label.rs`.
