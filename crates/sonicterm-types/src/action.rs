//! Bindable action enum + supporting direction/scroll types.
//!
//! These are the value shapes that flow from a parsed keymap into the app's
//! dispatcher. The loader/parser (`Keymap`, `Binding`, `Meta`) lives in
//! `sonicterm-core::keymap` because it pulls in `toml` + filesystem; the value
//! types live here so any crate can match on an `Action` without that
//! dependency.

use serde::{Deserialize, Serialize};

/// Direction for split/focus actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    /// Leftward.
    Left,
    /// Rightward.
    Right,
    /// Upward.
    Up,
    /// Downward.
    Down,
}

/// Broadcast input target scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BroadcastScope {
    /// Broadcast only to panes in the active tab.
    Tab,
    /// Broadcast to panes in every tab.
    AllTabs,
}

/// Scroll target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScrollAction {
    /// Scroll up by one line.
    LineUp,
    /// Scroll down by one line.
    LineDown,
    /// Scroll up by one page.
    PageUp,
    /// Scroll down by one page.
    PageDown,
    /// Jump to the top of scrollback.
    ToTop,
    /// Jump to the bottom (current screen).
    ToBottom,
}

/// All actions a binding may trigger. The renaming makes the TOML pretty.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    // Tabs
    /// Open a new tab.
    NewTab,
    /// Close the active tab.
    CloseTab,
    /// Activate the next tab.
    NextTab,
    /// Activate the previous tab.
    PrevTab,
    /// Activate the tab at the given zero-based index.
    ActivateTab(usize),
    /// Activate the last (rightmost) tab.
    ActivateLastTab,

    // Splits
    /// Split the active pane to the right.
    SplitRight,
    /// Split the active pane downward.
    SplitDown,
    /// Close the active pane.
    ClosePane,
    /// Context-aware close: if the active tab has more than one pane, close
    /// the active pane; otherwise close the active tab. iTerm2/wezterm-style
    /// Cmd+W semantic.
    CloseActivePaneOrTab,
    /// Temporarily make the active pane fill the tab area.
    TogglePaneZoom,
    /// Toggle broadcast input from the active pane to other panes.
    ToggleBroadcast {
        /// Which panes should receive mirrored input.
        scope: BroadcastScope,
    },
    /// Move focus to the pane in the given direction.
    FocusPane(Direction),
    /// Nudge the active split divider left.
    ResizePaneLeft,
    /// Nudge the active split divider right.
    ResizePaneRight,
    /// Nudge the active split divider up.
    ResizePaneUp,
    /// Nudge the active split divider down.
    ResizePaneDown,
    /// Resize the active pane.
    ResizePane {
        /// Direction to resize toward.
        dir: Direction,
        /// Number of cells to grow/shrink by.
        amount: u16,
    },

    // Clipboard
    /// Copy current selection to the system clipboard.
    CopyToClipboard,
    /// Enter keyboard-driven copy mode.
    EnterCopyMode,
    /// Enter URL quick-select hint mode.
    EnterQuickSelect,
    /// Paste from the system clipboard into the active pane.
    PasteFromClipboard,

    // Font
    /// Increase the configured font size by one step.
    IncreaseFontSize,
    /// Decrease the configured font size by one step.
    DecreaseFontSize,
    /// Reset the font size to the configured default.
    ResetFontSize,

    /// Apply a theme by name (live-applied; persists to config on next save).
    /// Bound from the View → Theme submenu in the macOS menubar.
    ApplyTheme(String),

    // UI chrome
    /// Toggle visibility of the tab bar.
    ToggleTabBar,
    /// Rename the active tab title body.
    RenameTab,

    // Window
    /// Open a new top-level window.
    NewWindow,
    /// Toggle fullscreen on the active window.
    ToggleFullscreen,

    // Search / palette
    /// Open the in-pane search overlay.
    OpenSearch,
    /// Open the command palette overlay.
    OpenCommandPalette,
    /// Open the editable user config TOML file in the OS default editor.
    EditConfigFile,
    /// Open the editable user keymap TOML file in the OS default editor.
    OpenKeymapFile,

    /// Scroll the active pane.
    Scroll(ScrollAction),

    // Shell integration (OSC 133)
    /// Jump to the previous shell prompt (OSC 133 mark).
    ScrollToPrevPrompt,
    /// Jump to the next shell prompt (OSC 133 mark).
    ScrollToNextPrompt,

    /// Reload the user configuration file from disk.
    ReloadConfig,

    /// Open a new pane connected to a remote shell over SSH. Argument is
    /// a `user@host[:port]` target string; parsing/validation happens in
    /// `sonicterm_io::ssh::parse_target` before any connection attempt.
    OpenSshPane(String),
}
