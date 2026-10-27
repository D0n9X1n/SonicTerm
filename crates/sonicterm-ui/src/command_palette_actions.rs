//! Canonical command actions and their fallback display names.

use super::*;

/// Backwards-compatible display name. The palette overlay rendering
/// now prefers the friendlier [`crate::command_label::label`], but
/// existing callers/tests that asked for `"NewTab"` (PascalCase
/// variant name) still get that here.
pub fn action_display_name(action: &Action) -> String {
    match action {
        Action::NewTab => "NewTab".into(),
        Action::CloseTab => "CloseTab".into(),
        Action::CloseActivePaneOrTab => "CloseActivePaneOrTab".into(),
        Action::NextTab => "NextTab".into(),
        Action::PrevTab => "PrevTab".into(),
        Action::ActivateTab(index) => format!("ActivateTab({index})"),
        Action::ActivateLastTab => "ActivateLastTab".into(),
        Action::SplitRight => "SplitRight".into(),
        Action::SplitDown => "SplitDown".into(),
        Action::ClosePane => "ClosePane".into(),
        Action::TogglePaneZoom => "TogglePaneZoom".into(),
        Action::ToggleBroadcast { scope } => {
            format!("ToggleBroadcast({})", broadcast_scope_name(*scope))
        }
        Action::FocusPane(direction) => format!("FocusPane({})", dir_name(*direction)),
        Action::ResizePaneLeft => "ResizePaneLeft".into(),
        Action::ResizePaneRight => "ResizePaneRight".into(),
        Action::ResizePaneUp => "ResizePaneUp".into(),
        Action::ResizePaneDown => "ResizePaneDown".into(),
        Action::ResizePane { dir, amount } => {
            format!("ResizePane({}, {amount})", dir_name(*dir))
        }
        Action::CopyToClipboard => "CopyToClipboard".into(),
        Action::EnterCopyMode => "EnterCopyMode".into(),
        Action::EnterQuickSelect => "EnterQuickSelect".into(),
        Action::PasteFromClipboard => "PasteFromClipboard".into(),
        Action::IncreaseFontSize => "IncreaseFontSize".into(),
        Action::DecreaseFontSize => "DecreaseFontSize".into(),
        Action::ResetFontSize => "ResetFontSize".into(),
        Action::IncreaseFontWeight => "IncreaseFontWeight".into(),
        Action::DecreaseFontWeight => "DecreaseFontWeight".into(),
        Action::ResetFontWeight => "ResetFontWeight".into(),
        Action::SaveCurrentSettings => "SaveCurrentSettings".into(),
        Action::ChooseFontFamily => "ChooseFontFamily".into(),
        Action::NewWindow => "NewWindow".into(),
        Action::MoveTabToNewWindow => "MoveTabToNewWindow".into(),
        Action::ToggleFullscreen => "ToggleFullscreen".into(),
        Action::QuitApp => "QuitApp".into(),
        Action::OpenSearch => "OpenSearch".into(),
        Action::OpenCommandPalette => "OpenCommandPalette".into(),
        Action::EditConfigFile => "EditConfigFile".into(),
        Action::OpenKeymapFile => "OpenKeymapFile".into(),
        Action::CheckForUpdates => "CheckForUpdates".into(),
        Action::Scroll(scroll) => format!("Scroll({})", scroll_name(*scroll)),
        Action::ScrollToPrevPrompt => "ScrollToPrevPrompt".into(),
        Action::ScrollToNextPrompt => "ScrollToNextPrompt".into(),
        Action::ReloadConfig => "ReloadConfig".into(),
        Action::ShowMemoryUsage => "ShowMemoryUsage".into(),
        Action::CopyMemoryReport => "CopyMemoryReport".into(),
        Action::ShowAbout => "ShowAbout".into(),
        Action::SetDefaultHandlers => "SetDefaultHandlers".into(),
        Action::ApplyTheme(name) => format!("ApplyTheme({name})"),
        Action::ToggleTabBar => "ToggleTabBar".into(),
        Action::ToggleStatusLine => "ToggleStatusLine".into(),
        Action::ToggleStatusLineSensitive => "ToggleStatusLineSensitive".into(),
        Action::StatusLinePositionTop => "StatusLinePositionTop".into(),
        Action::StatusLinePositionBottom => "StatusLinePositionBottom".into(),
        Action::IncreaseStatusLineFontScale => "IncreaseStatusLineFontScale".into(),
        Action::DecreaseStatusLineFontScale => "DecreaseStatusLineFontScale".into(),
        Action::ResetStatusLineFontScale => "ResetStatusLineFontScale".into(),
        Action::ChooseStatusLineSegments => "ChooseStatusLineSegments".into(),
        Action::FocusStatusLine => "FocusStatusLine".into(),
        Action::RenameTab => "RenameTab".into(),
        Action::RenameWindow => "RenameWindow".into(),
        Action::UpdateTabColor => "UpdateTabColor".into(),
    }
}

fn broadcast_scope_name(scope: sonicterm_cfg::keymap::BroadcastScope) -> &'static str {
    match scope {
        sonicterm_cfg::keymap::BroadcastScope::Tab => "Tab",
        sonicterm_cfg::keymap::BroadcastScope::AllTabs => "AllTabs",
    }
}

fn dir_name(direction: Direction) -> &'static str {
    match direction {
        Direction::Left => "Left",
        Direction::Right => "Right",
        Direction::Up => "Up",
        Direction::Down => "Down",
    }
}

fn scroll_name(scroll: ScrollAction) -> &'static str {
    match scroll {
        ScrollAction::LineUp => "LineUp",
        ScrollAction::LineDown => "LineDown",
        ScrollAction::PageUp => "PageUp",
        ScrollAction::PageDown => "PageDown",
        ScrollAction::ToTop => "ToTop",
        ScrollAction::ToBottom => "ToBottom",
    }
}

/// Canonical list of every bindable action variant. Parameterized actions use
/// representative arguments here for label/coverage tests; the command palette
/// uses [`palette_actions`] so it does not expose placeholder commands.
pub fn all_actions() -> Vec<Action> {
    let mut actions = palette_actions();
    actions.push(Action::ApplyTheme("wezterm".into()));
    // The palette lists About as its own entry, so the action is bindable but not listed twice.
    actions.push(Action::ShowAbout);
    actions
}

/// Canonical list of directly runnable palette actions, in the order the
/// palette should present them when no query is entered. Keep grouped by
/// feature area for readability. Theme actions are added only when they come
/// from the user's concrete keymap binding; SSH is hidden until its pane backend
/// is wired.
pub fn palette_actions() -> Vec<Action> {
    vec![
        // Tabs
        Action::NewTab,
        Action::CloseTab,
        Action::CloseActivePaneOrTab,
        Action::NextTab,
        Action::PrevTab,
        Action::ActivateLastTab,
        Action::ActivateTab(0),
        // Splits
        Action::SplitRight,
        Action::SplitDown,
        Action::ClosePane,
        Action::TogglePaneZoom,
        Action::ToggleBroadcast { scope: sonicterm_cfg::keymap::BroadcastScope::Tab },
        Action::ToggleBroadcast { scope: sonicterm_cfg::keymap::BroadcastScope::AllTabs },
        Action::FocusPane(Direction::Left),
        Action::FocusPane(Direction::Right),
        Action::FocusPane(Direction::Up),
        Action::FocusPane(Direction::Down),
        Action::ResizePaneLeft,
        Action::ResizePaneRight,
        Action::ResizePaneUp,
        Action::ResizePaneDown,
        Action::ResizePane { dir: Direction::Left, amount: 5 },
        Action::ResizePane { dir: Direction::Right, amount: 5 },
        Action::ResizePane { dir: Direction::Up, amount: 5 },
        Action::ResizePane { dir: Direction::Down, amount: 5 },
        // Clipboard
        Action::CopyToClipboard,
        Action::EnterCopyMode,
        Action::EnterQuickSelect,
        Action::PasteFromClipboard,
        // Font
        Action::IncreaseFontSize,
        Action::DecreaseFontSize,
        Action::ResetFontSize,
        Action::IncreaseFontWeight,
        Action::DecreaseFontWeight,
        Action::ResetFontWeight,
        Action::SaveCurrentSettings,
        Action::ChooseFontFamily,
        // UI chrome
        Action::ToggleTabBar,
        Action::ToggleStatusLine,
        Action::ToggleStatusLineSensitive,
        Action::StatusLinePositionTop,
        Action::StatusLinePositionBottom,
        Action::IncreaseStatusLineFontScale,
        Action::DecreaseStatusLineFontScale,
        Action::ResetStatusLineFontScale,
        Action::ChooseStatusLineSegments,
        Action::FocusStatusLine,
        Action::RenameTab,
        Action::UpdateTabColor,
        // Window
        Action::RenameWindow,
        Action::NewWindow,
        Action::MoveTabToNewWindow,
        Action::ToggleFullscreen,
        Action::QuitApp,
        // Search / palette / editable config files
        Action::OpenSearch,
        Action::OpenCommandPalette,
        Action::EditConfigFile,
        Action::OpenKeymapFile,
        Action::CheckForUpdates,
        // Scroll
        Action::Scroll(ScrollAction::LineUp),
        Action::Scroll(ScrollAction::LineDown),
        Action::Scroll(ScrollAction::PageUp),
        Action::Scroll(ScrollAction::PageDown),
        Action::Scroll(ScrollAction::ToTop),
        Action::Scroll(ScrollAction::ToBottom),
        // Shell integration
        Action::ScrollToPrevPrompt,
        Action::ScrollToNextPrompt,
        // Config
        Action::ReloadConfig,
        // Diagnostics
        Action::ShowMemoryUsage,
        Action::CopyMemoryReport,
        // System integration
        Action::SetDefaultHandlers,
    ]
}

/// Coverage assertion: every variant kind from
/// [`ALL_VARIANT_KINDS`] is represented by at least one entry in
/// [`all_actions`]. Lives here (not in the test crate) so the public
/// invariant is documented next to the data.
#[must_use]
pub fn covers_every_variant_kind() -> bool {
    use crate::command_label::variant_kind;
    let universe = all_actions();
    ALL_VARIANT_KINDS.iter().all(|kind| universe.iter().any(|action| variant_kind(action) == *kind))
}

#[cfg(test)]
#[path = "command_palette_actions_tests.rs"]
mod command_palette_actions_tests;
