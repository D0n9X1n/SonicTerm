//! Human-readable labels and keybinding hints for every
//! [`sonicterm_cfg::keymap::Action`] variant.
//!
//! Used by the command palette to render entries in a familiar
//! "Verb Noun" style (e.g. "New Tab", "Split Pane Right") instead of
//! the raw `PascalCase` variant names. The labels are also the fuzzy-
//! match haystack — typing "newtab" or "new t" or "n t" should all
//! land on `Action::NewTab`.
//!
//! Adding a new bindable action is a four-step process:
//!
//! 1. Add a variant to [`sonicterm_cfg::keymap::Action`].
//! 2. Add a match arm in [`label`] returning a `&'static str` or a
//!    formatted `String`.
//! 3. Add a discriminant entry in [`ALL_VARIANT_KINDS`] so label coverage
//!    stays exhaustive.
//! 4. Add a dispatch arm in `sonicterm_app::app::App::run_action`.
//!
//! The compile-time `match` in [`label`] guarantees we cannot forget
//! step 2 — the build breaks until every variant has a label. Step 3
//! is covered by the action-label coverage test.

use sonicterm_cfg::keymap::{Action, Direction, Keymap, ScrollAction};

/// Stable identifier for each Action variant kind. Used to enumerate
/// the universe of palette commands and to assert exhaustiveness.
///
/// `&'static str` instead of a separate enum keeps the variant list
/// human-greppable and avoids a parallel type that has to be kept in
/// sync with [`Action`] by hand.
pub const ALL_VARIANT_KINDS: &[&str] = &[
    "NewTab",
    "CloseTab",
    "CloseActivePaneOrTab",
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
    "EnterQuickSelect",
    "PasteFromClipboard",
    "IncreaseFontSize",
    "DecreaseFontSize",
    "ResetFontSize",
    "IncreaseFontWeight",
    "DecreaseFontWeight",
    "ResetFontWeight",
    "SaveCurrentSettings",
    "ApplyTheme",
    "ToggleTabBar",
    "RenameTab",
    "RenameWindow",
    "UpdateTabColor",
    "NewWindow",
    "MoveTabToNewWindow",
    "ToggleFullscreen",
    "QuitApp",
    "OpenSearch",
    "OpenCommandPalette",
    "EditConfigFile",
    "OpenKeymapFile",
    "CheckForUpdates",
    "Scroll",
    "ScrollToPrevPrompt",
    "ScrollToNextPrompt",
    "ReloadConfig",
    "OpenSshPane",
];

/// Stable command grouping independent of translated display text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum CommandCategory {
    /// Tab creation, selection, and appearance.
    Tabs,
    /// Pane layout and input distribution.
    Panes,
    /// Copy, paste, and selection modes.
    Clipboard,
    /// Font and window-chrome appearance.
    Appearance,
    /// Native window and application lifecycle.
    Window,
    /// Search and terminal-history navigation.
    Navigation,
    /// Configuration and maintenance.
    Settings,
}

impl CommandCategory {
    /// Canonical order for command groups.
    pub const ALL: [Self; 7] = [
        Self::Tabs,
        Self::Panes,
        Self::Clipboard,
        Self::Appearance,
        Self::Window,
        Self::Navigation,
        Self::Settings,
    ];

    /// Localized group name with an English fallback.
    pub fn label(self, i18n: &crate::i18n::I18n) -> String {
        let (key, fallback) = self.message();
        i18n.try_t_args(key, None).unwrap_or_else(|| fallback.to_string())
    }

    pub(crate) fn message(self) -> (&'static str, &'static str) {
        match self {
            Self::Tabs => ("command-category-tabs", "Tabs"),
            Self::Panes => ("command-category-panes", "Panes"),
            Self::Clipboard => ("command-category-clipboard", "Clipboard"),
            Self::Appearance => ("command-category-appearance", "Appearance"),
            Self::Window => ("command-category-window", "Window"),
            Self::Navigation => ("command-category-navigation", "Navigation"),
            Self::Settings => ("command-category-settings", "Settings"),
        }
    }
}

/// Window-local facts used to explain whether a command has a current target.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct CommandContext {
    /// A live terminal window owns the palette.
    pub window_available: bool,
    /// Number of tabs in that window.
    pub tab_count: usize,
    /// Its active tab has a registered active pane.
    pub pane_available: bool,
    /// A nonempty selection belongs to that pane.
    pub selection_available: bool,
    /// The window is in READONLY mode.
    pub read_only: bool,
    /// Live focus neighbors in left, right, up, down order.
    pub focus_available: [bool; 4],
}

/// Target required by an action independently of its keybinding or execution owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandRequirement {
    /// No existing terminal target is required.
    Global,
    /// A terminal window is required.
    Window,
    /// An active tab is required.
    Tab,
    /// An active pane is required.
    Pane,
    /// A nonempty active-pane selection is required.
    Selection,
    /// The action's positional tab index must exist.
    TabIndex,
    /// The action's focus direction must reach another live pane.
    FocusNeighbor,
}

/// Explanation for a command that cannot execute in its current context.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DisabledReason {
    /// No live terminal window owns the command.
    NoWindow,
    /// The window has no active tab.
    NoTab,
    /// The active pane is unavailable.
    NoPane,
    /// The active pane has no nonempty selection.
    NoSelection,
    /// READONLY policy excludes the action.
    ReadOnly,
    /// The requested tab index is absent.
    MissingTab,
    /// The requested direction has no live neighbor.
    NoNeighbor,
}

impl DisabledReason {
    pub(crate) const ALL: [Self; 7] = [
        Self::NoWindow,
        Self::NoTab,
        Self::NoPane,
        Self::NoSelection,
        Self::ReadOnly,
        Self::MissingTab,
        Self::NoNeighbor,
    ];

    pub(crate) fn message(self) -> (&'static str, &'static str) {
        match self {
            Self::NoWindow => ("command-disabled-window", "No terminal window"),
            Self::NoTab => ("command-disabled-tab", "No active tab"),
            Self::NoPane => ("command-disabled-pane", "No active pane"),
            Self::NoSelection => ("command-disabled-selection", "No text selected"),
            Self::ReadOnly => ("command-disabled-readonly", "Unavailable in READONLY mode"),
            Self::MissingTab => ("command-disabled-tab-index", "Tab no longer exists"),
            Self::NoNeighbor => ("command-disabled-neighbor", "No pane in that direction"),
        }
    }
}

/// Evaluate target availability without executing an action or inspecting terminal payloads.
#[must_use]
pub fn disabled_reason(action: &Action, context: &CommandContext) -> Option<DisabledReason> {
    let entry = descriptor(action);
    if context.read_only && !entry.read_only_allowed {
        // When: `read_only` excludes this descriptor, no target fact can authorize execution.
        return Some(DisabledReason::ReadOnly);
    }
    if entry.requirement == CommandRequirement::Global {
        // When: `requirement` is Global, the action does not need an existing terminal target.
        return None;
    }
    if !context.window_available {
        // When: `window_available` is false, commands cannot borrow another window's target.
        return Some(DisabledReason::NoWindow);
    }
    if entry.requirement == CommandRequirement::Window {
        // When: `requirement` is Window, an empty terminal window is still a valid target.
        return None;
    }
    if context.tab_count == 0 {
        // When: `tab_count` is zero, no active tab or pane can receive the command.
        return Some(DisabledReason::NoTab);
    }
    if entry.requirement == CommandRequirement::Tab {
        // When: `requirement` is Tab, pane availability does not affect this command.
        return None;
    }
    if let Action::ActivateTab(index) = action {
        // When: `action` names a positional tab, validate its index against this window only.
        return (*index >= context.tab_count).then_some(DisabledReason::MissingTab);
    }
    if !context.pane_available {
        // When: `pane_available` is false, pane-local commands have no live target.
        return Some(DisabledReason::NoPane);
    }
    match action {
        Action::CopyToClipboard if !context.selection_available => {
            Some(DisabledReason::NoSelection)
        }
        Action::FocusPane(direction) => {
            let index = match direction {
                Direction::Left => 0,
                Direction::Right => 1,
                Direction::Up => 2,
                Direction::Down => 3,
            };
            (!context.focus_available[index]).then_some(DisabledReason::NoNeighbor)
        }
        _ => None,
    }
}

/// Typed metadata and target policy for an action; execution remains app-owned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandDescriptor {
    /// Stable existing action-variant identity, not a translated name.
    pub id: &'static str,
    /// Canonical command group.
    pub category: CommandCategory,
    /// Fluent message key for this action's label template.
    pub label_key: &'static str,
    /// Existing English search aliases in canonical order.
    pub aliases: &'static [&'static str],
    /// Window-local target facts needed before execution.
    pub requirement: CommandRequirement,
    /// Whether READONLY permits this action regardless of its binding.
    pub read_only_allowed: bool,
}

/// Describe an action without copying its runtime arguments or deciding where it executes.
#[must_use]
pub fn descriptor(action: &Action) -> CommandDescriptor {
    use CommandCategory::*;
    let (id, category, label_key, aliases): (_, _, _, &'static [&'static str]) = match action {
        Action::NewTab => ("NewTab", Tabs, "command-new-tab", &["create", "open"]),
        Action::CloseTab => ("CloseTab", Tabs, "command-close-tab", &["quit", "x"]),
        Action::CloseActivePaneOrTab => (
            "CloseActivePaneOrTab",
            Tabs,
            "command-close-pane-or-tab",
            &["close", "quit", "x", "pane", "tab", "cmd+w"],
        ),
        Action::NextTab => ("NextTab", Tabs, "command-next-tab", &["forward", "right"]),
        Action::PrevTab => ("PrevTab", Tabs, "command-prev-tab", &["back", "left", "previous"]),
        Action::ActivateTab(_) => ("ActivateTab", Tabs, "command-activate-tab", &["switch", "go"]),
        Action::ActivateLastTab => {
            ("ActivateLastTab", Tabs, "command-activate-last-tab", &["recent", "switch"])
        }
        Action::SplitRight => {
            ("SplitRight", Panes, "command-split-right", &["pane", "vertical", "vsplit"])
        }
        Action::SplitDown => {
            ("SplitDown", Panes, "command-split-down", &["pane", "horizontal", "hsplit"])
        }
        Action::ClosePane => ("ClosePane", Panes, "command-close-pane", &["kill", "x"]),
        Action::TogglePaneZoom => {
            ("TogglePaneZoom", Panes, "command-toggle-pane-zoom", &["pane", "maximize", "unzoom"])
        }
        Action::ToggleBroadcast { .. } => (
            "ToggleBroadcast",
            Panes,
            "command-toggle-broadcast",
            &["broadcast", "input", "mirror", "all panes", "all tabs"],
        ),
        Action::FocusPane(_) => {
            ("FocusPane", Panes, "command-focus-pane", &["move", "switch", "navigate"])
        }
        Action::ResizePaneLeft => (
            "ResizePaneLeft",
            Panes,
            "command-resize-pane-left",
            &["grow", "shrink", "nudge", "divider"],
        ),
        Action::ResizePaneRight => (
            "ResizePaneRight",
            Panes,
            "command-resize-pane-right",
            &["grow", "shrink", "nudge", "divider"],
        ),
        Action::ResizePaneUp => (
            "ResizePaneUp",
            Panes,
            "command-resize-pane-up",
            &["grow", "shrink", "nudge", "divider"],
        ),
        Action::ResizePaneDown => (
            "ResizePaneDown",
            Panes,
            "command-resize-pane-down",
            &["grow", "shrink", "nudge", "divider"],
        ),
        Action::ResizePane { .. } => {
            ("ResizePane", Panes, "command-resize-pane", &["grow", "shrink", "nudge", "divider"])
        }
        Action::CopyToClipboard => ("CopyToClipboard", Clipboard, "command-copy", &["yank"]),
        Action::EnterCopyMode => (
            "EnterCopyMode",
            Clipboard,
            "command-copy-mode",
            &["keyboard", "selection", "yank", "vim", "readonly", "read only"],
        ),
        Action::EnterQuickSelect => (
            "EnterQuickSelect",
            Clipboard,
            "command-quick-select",
            &["url", "hint", "keyboard", "yank"],
        ),
        Action::PasteFromClipboard => ("PasteFromClipboard", Clipboard, "command-paste", &["yank"]),
        Action::IncreaseFontSize => (
            "IncreaseFontSize",
            Appearance,
            "command-increase-font-size",
            &["bigger", "zoom in", "larger"],
        ),
        Action::DecreaseFontSize => {
            ("DecreaseFontSize", Appearance, "command-decrease-font-size", &["smaller", "zoom out"])
        }
        Action::ResetFontSize => {
            ("ResetFontSize", Appearance, "command-reset-font-size", &["default", "zoom reset"])
        }
        Action::IncreaseFontWeight => (
            "IncreaseFontWeight",
            Appearance,
            "command-increase-font-weight",
            &["bolder", "bolder font", "bold font", "heavier", "thicker", "bold", "weight"],
        ),
        Action::DecreaseFontWeight => (
            "DecreaseFontWeight",
            Appearance,
            "command-decrease-font-weight",
            &["thinner", "thinner font", "light font", "lighter", "slimmer", "weight"],
        ),
        Action::ResetFontWeight => (
            "ResetFontWeight",
            Appearance,
            "command-reset-font-weight",
            &["weight", "default", "native", "normal font"],
        ),
        Action::SaveCurrentSettings => (
            "SaveCurrentSettings",
            Settings,
            "command-save-settings",
            &[
                "save",
                "persist",
                "current",
                "settings",
                "config",
                "preferences",
                "font",
                "zoom",
                "weight",
            ],
        ),
        Action::ApplyTheme(_) => (
            "ApplyTheme",
            Appearance,
            "command-apply-theme",
            &["color", "colors", "colour", "scheme", "appearance"],
        ),
        Action::ToggleTabBar => {
            ("ToggleTabBar", Appearance, "command-toggle-tab-bar", &["hide", "show"])
        }
        Action::RenameTab => {
            ("RenameTab", Tabs, "command-rename-tab", &["title", "name", "label", "edit"])
        }
        Action::UpdateTabColor => (
            "UpdateTabColor",
            Tabs,
            "command-tab-color",
            &["tab", "color", "colour", "accent", "theme"],
        ),
        Action::RenameWindow => {
            ("RenameWindow", Window, "command-rename-window", &["title", "name", "label", "edit"])
        }
        Action::NewWindow => ("NewWindow", Window, "command-new-window", &["create", "open"]),
        Action::MoveTabToNewWindow => (
            "MoveTabToNewWindow",
            Window,
            "command-move-tab-window",
            &["detach", "tear out", "pop out", "separate"],
        ),
        Action::ToggleFullscreen => {
            ("ToggleFullscreen", Window, "command-fullscreen", &["maximize", "full"])
        }
        Action::QuitApp => (
            "QuitApp",
            Window,
            "command-quit",
            &["quit", "exit", "close app", "cmd+q", "terminate"],
        ),
        Action::OpenSearch => ("OpenSearch", Navigation, "command-search", &["find"]),
        Action::OpenCommandPalette => {
            ("OpenCommandPalette", Navigation, "command-palette", &["palette", "commands"])
        }
        Action::EditConfigFile => (
            "EditConfigFile",
            Settings,
            "command-edit-config",
            &["settings", "config", "options", "prefs", "preferences", "toml"],
        ),
        Action::OpenKeymapFile => (
            "OpenKeymapFile",
            Settings,
            "command-edit-keymap",
            &["settings", "config", "keys", "bindings", "shortcuts", "toml"],
        ),
        Action::CheckForUpdates => (
            "CheckForUpdates",
            Settings,
            "command-check-updates",
            &["update", "updates", "upgrade", "release", "version"],
        ),
        Action::Scroll(_) => {
            ("Scroll", Navigation, "command-scroll", &["page", "line", "scrollback"])
        }
        Action::ScrollToPrevPrompt => (
            "ScrollToPrevPrompt",
            Navigation,
            "command-prev-prompt",
            &["jump", "prompt", "previous"],
        ),
        Action::ScrollToNextPrompt => {
            ("ScrollToNextPrompt", Navigation, "command-next-prompt", &["jump", "prompt", "next"])
        }
        Action::ReloadConfig => {
            ("ReloadConfig", Settings, "command-reload-config", &["refresh", "config", "settings"])
        }
        Action::OpenSshPane(_) => {
            ("OpenSshPane", Panes, "command-ssh-pane", &["remote", "connect"])
        }
    };
    use CommandRequirement as Requirement;
    let (requirement, read_only_allowed) = match action {
        Action::NewWindow
        | Action::QuitApp
        | Action::EditConfigFile
        | Action::OpenKeymapFile
        | Action::ReloadConfig
        | Action::ApplyTheme(_) => (Requirement::Global, false),
        Action::CheckForUpdates | Action::SaveCurrentSettings => (Requirement::Global, true),
        Action::NewTab
        | Action::IncreaseFontSize
        | Action::DecreaseFontSize
        | Action::ResetFontSize
        | Action::IncreaseFontWeight
        | Action::DecreaseFontWeight
        | Action::ResetFontWeight
        | Action::ToggleTabBar
        | Action::ToggleFullscreen => (Requirement::Window, false),
        Action::OpenCommandPalette | Action::RenameWindow => (Requirement::Window, true),
        Action::CloseTab
        | Action::CloseActivePaneOrTab
        | Action::RenameTab
        | Action::UpdateTabColor
        | Action::MoveTabToNewWindow => (Requirement::Tab, false),
        Action::NextTab | Action::PrevTab | Action::ActivateLastTab => (Requirement::Tab, true),
        Action::ActivateTab(_) => (Requirement::TabIndex, true),
        Action::FocusPane(_) => (Requirement::FocusNeighbor, true),
        Action::CopyToClipboard => (Requirement::Selection, false),
        Action::OpenSearch => (Requirement::Pane, true),
        Action::SplitRight
        | Action::SplitDown
        | Action::ClosePane
        | Action::TogglePaneZoom
        | Action::ToggleBroadcast { .. }
        | Action::ResizePaneLeft
        | Action::ResizePaneRight
        | Action::ResizePaneUp
        | Action::ResizePaneDown
        | Action::ResizePane { .. }
        | Action::EnterCopyMode
        | Action::EnterQuickSelect
        | Action::PasteFromClipboard
        | Action::Scroll(_)
        | Action::ScrollToPrevPrompt
        | Action::ScrollToNextPrompt
        | Action::OpenSshPane(_) => (Requirement::Pane, false),
    };
    CommandDescriptor { id, category, label_key, aliases, requirement, read_only_allowed }
}

/// Return the stable action-variant identity represented in the command catalog.
#[must_use]
pub fn variant_kind(a: &Action) -> &'static str {
    descriptor(a).id
}

/// Render a human-readable label for the palette. The format is
/// "Verb Noun" so fuzzy matching against a typed query like
/// "split right" or "new tab" feels natural.
#[must_use]
pub fn label(a: &Action) -> String {
    match a {
        Action::NewTab => "New Tab".into(),
        Action::CloseTab => "Close Tab".into(),
        Action::CloseActivePaneOrTab => "Close Pane or Tab".into(),
        Action::NextTab => "Next Tab".into(),
        Action::PrevTab => "Previous Tab".into(),
        Action::ActivateTab(i) => format!("Activate Tab {}", i + 1),
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
        Action::EnterCopyMode => "Enter Read Only Mode".into(),
        Action::EnterQuickSelect => "Enter Quick Select".into(),
        Action::PasteFromClipboard => "Paste from Clipboard".into(),
        Action::IncreaseFontSize => "Increase Font Size".into(),
        Action::DecreaseFontSize => "Decrease Font Size".into(),
        Action::ResetFontSize => "Reset Font Size".into(),
        Action::IncreaseFontWeight => "Increase Font Weight (Bolder)".into(),
        Action::DecreaseFontWeight => "Decrease Font Weight (Thinner)".into(),
        Action::ResetFontWeight => "Reset Font Weight to Config".into(),
        Action::SaveCurrentSettings => "Save Current Settings".into(),
        Action::ApplyTheme(name) => format!("Apply Theme: {name}"),
        Action::ToggleTabBar => "Toggle Tab Bar".into(),
        Action::RenameTab => "Rename Active Tab".into(),
        Action::RenameWindow => "Rename Window".into(),
        Action::UpdateTabColor => "Update Tab Color".into(),
        Action::NewWindow => "New Window".into(),
        Action::MoveTabToNewWindow => "Move Tab to New Window".into(),
        Action::ToggleFullscreen => "Toggle Fullscreen".into(),
        Action::QuitApp => "Quit SonicTerm".into(),
        Action::OpenSearch => "Open Search".into(),
        Action::OpenCommandPalette => "Open Command Palette".into(),
        Action::EditConfigFile => "Edit sonicterm.toml".into(),
        Action::OpenKeymapFile => "Edit keymap.toml".into(),
        Action::CheckForUpdates => "Check for Updates".into(),
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
/// [`Action::EditConfigFile`] even though its label is
/// "Edit sonicterm.toml" (no `sett` subsequence). We expose
/// `["settings", "config", "options", "prefs"]` so any of those land it.
#[must_use]
pub fn keywords(a: &Action) -> &'static [&'static str] {
    descriptor(a).aliases
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

/// Render a translated command template while retaining the existing English fallback.
#[must_use]
pub fn localized_label(action: &Action, i18n: &crate::i18n::I18n) -> String {
    let entry = descriptor(action);
    let mut args = Vec::new();
    let numeric;
    match action {
        Action::ActivateTab(index) => {
            numeric = (index + 1).to_string();
            args.push(("number", numeric.as_str()));
        }
        Action::FocusPane(direction) => args.push(("direction", direction_token(*direction))),
        Action::ResizePane { dir, amount } => {
            numeric = amount.to_string();
            args.push(("direction", direction_token(*dir)));
            args.push(("amount", numeric.as_str()));
        }
        Action::ToggleBroadcast { scope } => {
            let token = match scope {
                sonicterm_cfg::keymap::BroadcastScope::Tab => "tab",
                sonicterm_cfg::keymap::BroadcastScope::AllTabs => "all",
            };
            args.push(("scope", token));
        }
        Action::ApplyTheme(name) => args.push(("name", name.as_str())),
        Action::OpenSshPane(target) => args.push(("target", target.as_str())),
        Action::Scroll(target) => {
            let token = match target {
                ScrollAction::LineUp => "line-up",
                ScrollAction::LineDown => "line-down",
                ScrollAction::PageUp => "page-up",
                ScrollAction::PageDown => "page-down",
                ScrollAction::ToTop => "top",
                ScrollAction::ToBottom => "bottom",
            };
            args.push(("target", token));
        }
        _ => {
            // When: `action` has no runtime arguments, its complete text comes from the template.
        }
    }
    i18n.try_t_args(entry.label_key, Some(&args)).unwrap_or_else(|| label(action))
}

fn direction_token(direction: Direction) -> &'static str {
    match direction {
        Direction::Left => "left",
        Direction::Right => "right",
        Direction::Up => "up",
        Direction::Down => "down",
    }
}

/// Keep English labels and aliases searchable alongside translated command text.
#[must_use]
pub fn localized_search_haystack(action: &Action, i18n: &crate::i18n::I18n) -> String {
    let translated = localized_label(action, i18n);
    let english = search_haystack(action);
    if translated == label(action) {
        english
    } else {
        // When: `translated` differs from `label(action)`, retain both spellings for localized and English queries.
        format!("{translated} {english}")
    }
}

fn broadcast_scope_human(scope: sonicterm_cfg::keymap::BroadcastScope) -> &'static str {
    match scope {
        sonicterm_cfg::keymap::BroadcastScope::Tab => "Tab",
        sonicterm_cfg::keymap::BroadcastScope::AllTabs => "All Tabs",
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
#[path = "command_label_tests.rs"]
mod command_label_tests;
