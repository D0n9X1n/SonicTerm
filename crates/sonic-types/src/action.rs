//! Bindable action enum + supporting direction/scroll types.
//!
//! These are the value shapes that flow from a parsed keymap into the app's
//! dispatcher. The loader/parser (`Keymap`, `Binding`, `Meta`) lives in
//! `sonic-core::keymap` because it pulls in `toml` + filesystem; the value
//! types live here so any crate can match on an `Action` without that
//! dependency.

use serde::{Deserialize, Serialize};

/// Direction for split/focus actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

/// Scroll target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScrollAction {
    LineUp,
    LineDown,
    PageUp,
    PageDown,
    ToTop,
    ToBottom,
}

/// All actions a binding may trigger. The renaming makes the TOML pretty.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    // Tabs
    NewTab,
    CloseTab,
    NextTab,
    PrevTab,
    ActivateTab(usize),
    ActivateLastTab,

    // Splits
    SplitRight,
    SplitDown,
    ClosePane,
    FocusPane(Direction),
    ResizePane {
        dir: Direction,
        amount: u16,
    },

    // Clipboard
    CopyToClipboard,
    PasteFromClipboard,

    // Font
    IncreaseFontSize,
    DecreaseFontSize,
    ResetFontSize,

    // Theme (live-apply by name; persists to config on next save).
    // Bound from the View → Theme submenu in the macOS menubar.
    ApplyTheme(String),

    // UI chrome
    ToggleTabBar,

    // Window
    NewWindow,
    ToggleFullscreen,

    // Search / palette
    OpenSearch,
    OpenCommandPalette,
    OpenPreferences,

    // Scroll
    Scroll(ScrollAction),

    // Shell integration (OSC 133)
    ScrollToPrevPrompt,
    ScrollToNextPrompt,

    // Config
    ReloadConfig,

    /// Open a new pane connected to a remote shell over SSH. Argument is
    /// a `user@host[:port]` target string; parsing/validation happens in
    /// `sonic_core::ssh::parse_target` before any connection attempt.
    OpenSshPane(String),
}
