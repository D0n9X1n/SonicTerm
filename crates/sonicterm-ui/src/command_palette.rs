//! Command palette (Cmd+Shift+P). Pure-data state holder.
//!
//! The palette is a fuzzy-searchable list of runnable
//! [`sonicterm_cfg::keymap::Action`] values. The keyboard-event handler in
//! [`crate::app`] routes printable characters, arrow keys, Enter and Esc
//! into this state instead of forwarding them to the active pty when
//! [`CommandPalette::is_open`] returns `true`. On Enter the dispatcher
//! reads [`CommandPalette::current`] and runs that action through
//! `App::run_action`.
//!
//! Filtering is now a VSCode-style fuzzy match using
//! [`nucleo_matcher`]: each candidate label gets a score, results are
//! sorted descending by score, and ties fall back to the canonical
//! order returned by [`all_actions`]. Empty query matches everything
//! in canonical order. The legacy subsequence behavior is preserved
//! as the underlying ranker (substring runs score above scattered
//! matches), so historical tests that depend on subsequence semantics
//! still pass.

use nucleo_matcher::{
    pattern::{CaseMatching, Normalization, Pattern},
    Config, Matcher, Utf32Str,
};
use sonicterm_cfg::keymap::{Action, Direction, Keymap, ScrollAction};

use crate::command_label::{keybinding_hint, search_haystack, ALL_VARIANT_KINDS};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandPaletteMode {
    Commands,
    RenameTab,
}

/// State for the command palette overlay. Owned by `App`.
#[derive(Debug, Clone)]
pub struct CommandPalette {
    open: bool,
    mode: CommandPaletteMode,
    query: String,
    /// Full universe of actions, in canonical order.
    all: Vec<Action>,
    /// First keybinding hint for each action in `all`, parallel order.
    shortcut_hints: Vec<Option<String>>,
    /// Filtered view — indices into `all` matched by the current query,
    /// or all indices when the query is empty. Order is descending
    /// fuzzy-score, with canonical-order tiebreak.
    items: Vec<usize>,
    selected: usize,
    tab_count: usize,
    /// First visible item index in the rendered viewport. Maintained by
    /// [`Self::ensure_selected_in_view`] so that arrow-key navigation
    /// keeps the highlighted row inside the modal even when the
    /// filtered list is longer than `visible_rows`.
    scroll_offset: usize,
    /// Cached count of rows the renderer can actually display, set via
    /// [`Self::set_visible_rows`]. Zero means "unconstrained" — used by
    /// tests that don't know the modal size yet.
    visible_rows: usize,
}

impl Default for CommandPalette {
    fn default() -> Self {
        Self::new()
    }
}

impl CommandPalette {
    pub fn new() -> Self {
        let all = palette_actions();
        let shortcut_hints = vec![None; all.len()];
        let items = (0..all.len()).collect();
        Self {
            open: false,
            mode: CommandPaletteMode::Commands,
            query: String::new(),
            all,
            shortcut_hints,
            items,
            selected: 0,
            tab_count: usize::MAX,
            scroll_offset: 0,
            visible_rows: 0,
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn mode(&self) -> CommandPaletteMode {
        self.mode
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    /// Visible action list (filtered). Display order is what the renderer
    /// should show.
    pub fn visible(&self) -> Vec<&Action> {
        if self.mode == CommandPaletteMode::RenameTab {
            return Vec::new();
        }
        self.items.iter().filter_map(|&i| self.all.get(i)).collect()
    }

    pub fn shortcut_hint_for_visible_index(&self, visible_index: usize) -> Option<&str> {
        let all_index = *self.items.get(visible_index)?;
        self.shortcut_hints.get(all_index)?.as_deref()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Open the palette and reset to a clean state.
    pub fn open(&mut self) {
        self.open = true;
        self.mode = CommandPaletteMode::Commands;
        self.query.clear();
        self.selected = 0;
        self.scroll_offset = 0;
        self.refilter();
    }

    pub fn close(&mut self) {
        self.open = false;
        self.mode = CommandPaletteMode::Commands;
        self.query.clear();
        self.selected = 0;
        self.scroll_offset = 0;
        self.refilter();
    }

    /// Toggle open/close. Returns the new open state.
    pub fn toggle(&mut self) -> bool {
        if self.open {
            self.close();
        } else {
            self.open();
        }
        self.open
    }

    pub fn set_query(&mut self, q: impl Into<String>) {
        self.query = q.into();
        self.selected = 0;
        self.scroll_offset = 0;
        if self.mode == CommandPaletteMode::Commands {
            self.refilter();
        }
    }

    pub fn set_keymap(&mut self, keymap: &Keymap) {
        self.all = palette_actions();
        for binding in &keymap.bindings {
            let action = &binding.action.0;
            if palette_accepts_keymap_action(action) && !self.all.contains(action) {
                self.all.push(action.clone());
            }
        }
        self.shortcut_hints =
            self.all.iter().map(|action| keybinding_hint(keymap, action)).collect();
        self.items = (0..self.all.len()).collect();
        self.selected = self.selected.min(self.items.len().saturating_sub(1));
        self.refilter();
    }

    pub fn set_tab_count(&mut self, tab_count: usize) {
        let tab_count = tab_count.max(1);
        if self.tab_count == tab_count {
            return;
        }
        self.tab_count = tab_count;
        self.selected = 0;
        self.scroll_offset = 0;
        if self.mode == CommandPaletteMode::Commands {
            self.refilter();
        }
    }

    pub fn input_char(&mut self, ch: char) {
        self.query.push(ch);
        self.selected = 0;
        self.scroll_offset = 0;
        if self.mode == CommandPaletteMode::Commands {
            self.refilter();
        }
    }

    pub fn backspace(&mut self) {
        if self.query.pop().is_some() {
            self.selected = 0;
            self.scroll_offset = 0;
            if self.mode == CommandPaletteMode::Commands {
                self.refilter();
            }
        }
    }

    pub fn start_rename_tab(&mut self, title_body: impl Into<String>) {
        self.open = true;
        self.mode = CommandPaletteMode::RenameTab;
        self.query = title_body.into();
        self.items.clear();
        self.selected = 0;
        self.scroll_offset = 0;
    }

    pub fn move_selection_down(&mut self) {
        if self.items.is_empty() {
            self.selected = 0;
            self.scroll_offset = 0;
            return;
        }
        self.selected = (self.selected + 1) % self.items.len();
        self.ensure_selected_in_view();
    }

    pub fn move_selection_up(&mut self) {
        if self.items.is_empty() {
            self.selected = 0;
            self.scroll_offset = 0;
            return;
        }
        self.selected = if self.selected == 0 { self.items.len() - 1 } else { self.selected - 1 };
        self.ensure_selected_in_view();
    }

    /// Current first-visible-row offset. The renderer uses this to draw
    /// only items `[scroll_offset .. scroll_offset + visible_rows]` and
    /// to position the highlight relative to that window.
    pub fn scroll_offset(&self) -> usize {
        self.scroll_offset
    }

    /// Number of rows the renderer can show. Set by the renderer once
    /// it knows the modal height (see [`crate::overlays::PaletteLayout`]).
    /// A zero value means "unconstrained" and disables clamping — useful
    /// only for tests; production layout always sets a concrete value.
    pub fn set_visible_rows(&mut self, rows: usize) {
        self.visible_rows = rows;
        self.ensure_selected_in_view();
    }

    pub fn visible_rows(&self) -> usize {
        self.visible_rows
    }

    /// Clamp `scroll_offset` so `selected` is always inside the
    /// `[scroll_offset, scroll_offset + visible_rows)` half-open window.
    /// When `visible_rows == 0` this is a no-op (no constraint known).
    fn ensure_selected_in_view(&mut self) {
        if self.visible_rows == 0 || self.items.is_empty() {
            return;
        }
        if self.selected < self.scroll_offset {
            self.scroll_offset = self.selected;
        } else if self.selected >= self.scroll_offset + self.visible_rows {
            self.scroll_offset = self.selected + 1 - self.visible_rows;
        }
        // Don't leave a trailing gap of empty rows at the bottom when the
        // list shrinks under us (post-refilter).
        let max_off = self.items.len().saturating_sub(self.visible_rows);
        if self.scroll_offset > max_off {
            self.scroll_offset = max_off;
        }
    }

    /// The currently highlighted action, if any.
    pub fn current(&self) -> Option<&Action> {
        if self.mode == CommandPaletteMode::RenameTab {
            return None;
        }
        self.items.get(self.selected).and_then(|&i| self.all.get(i))
    }

    /// Fuzzy-match `query` against the human label of each candidate
    /// action; sort hits descending by nucleo score with canonical-
    /// order tiebreak. Empty query is canonical order, full universe.
    fn refilter(&mut self) {
        if self.query.is_empty() {
            self.items =
                (0..self.all.len()).filter(|&i| self.action_available(&self.all[i])).collect();
        } else {
            let mut matcher = Matcher::new(Config::DEFAULT);
            let pattern = Pattern::parse(&self.query, CaseMatching::Ignore, Normalization::Smart);
            let mut scratch: Vec<char> = Vec::new();
            let mut scored: Vec<(usize, u32)> = self
                .all
                .iter()
                .enumerate()
                .filter(|(_, a)| self.action_available(a))
                .filter_map(|(i, a)| {
                    scratch.clear();
                    let mut label = search_haystack(a);
                    if let Some(Some(hint)) = self.shortcut_hints.get(i) {
                        label.push(' ');
                        label.push_str(hint);
                    }
                    let haystack = Utf32Str::new(&label, &mut scratch);
                    pattern.score(haystack, &mut matcher).map(|s| (i, s))
                })
                .collect();
            scored.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            self.items = scored.into_iter().map(|(i, _)| i).collect();
        }
        if self.selected >= self.items.len() {
            self.selected = 0;
        }
        self.ensure_selected_in_view();
    }

    fn action_available(&self, action: &Action) -> bool {
        !matches!(action, Action::ActivateTab(i) if *i >= self.tab_count)
    }
}

/// Backwards-compatible display name. The palette overlay rendering
/// now prefers the friendlier [`crate::command_label::label`], but
/// existing callers/tests that asked for `"NewTab"` (PascalCase
/// variant name) still get that here.
pub fn action_display_name(a: &Action) -> String {
    match a {
        Action::NewTab => "NewTab".into(),
        Action::CloseTab => "CloseTab".into(),
        Action::CloseActivePaneOrTab => "CloseActivePaneOrTab".into(),
        Action::NextTab => "NextTab".into(),
        Action::PrevTab => "PrevTab".into(),
        Action::ActivateTab(i) => format!("ActivateTab({i})"),
        Action::ActivateLastTab => "ActivateLastTab".into(),
        Action::SplitRight => "SplitRight".into(),
        Action::SplitDown => "SplitDown".into(),
        Action::ClosePane => "ClosePane".into(),
        Action::TogglePaneZoom => "TogglePaneZoom".into(),
        Action::ToggleBroadcast { scope } => {
            format!("ToggleBroadcast({})", broadcast_scope_name(*scope))
        }
        Action::FocusPane(d) => format!("FocusPane({})", dir_name(*d)),
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
        Action::NewWindow => "NewWindow".into(),
        Action::ToggleFullscreen => "ToggleFullscreen".into(),
        Action::OpenSearch => "OpenSearch".into(),
        Action::OpenCommandPalette => "OpenCommandPalette".into(),
        Action::ShowKeymapCheatsheet => "ShowKeymapCheatsheet".into(),
        Action::EditConfigFile => "EditConfigFile".into(),
        Action::OpenKeymapFile => "OpenKeymapFile".into(),
        Action::Scroll(s) => format!("Scroll({})", scroll_name(*s)),
        Action::ScrollToPrevPrompt => "ScrollToPrevPrompt".into(),
        Action::ScrollToNextPrompt => "ScrollToNextPrompt".into(),
        Action::ReloadConfig => "ReloadConfig".into(),
        Action::OpenSshPane(t) => format!("OpenSshPane({t})"),
        Action::ApplyTheme(name) => format!("ApplyTheme({name})"),
        Action::ToggleTabBar => "ToggleTabBar".into(),
        Action::RenameTab => "RenameTab".into(),
    }
}

fn broadcast_scope_name(scope: sonicterm_cfg::keymap::BroadcastScope) -> &'static str {
    match scope {
        sonicterm_cfg::keymap::BroadcastScope::Tab => "Tab",
        sonicterm_cfg::keymap::BroadcastScope::AllTabs => "AllTabs",
    }
}

fn dir_name(d: Direction) -> &'static str {
    match d {
        Direction::Left => "Left",
        Direction::Right => "Right",
        Direction::Up => "Up",
        Direction::Down => "Down",
    }
}

fn scroll_name(s: ScrollAction) -> &'static str {
    match s {
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
    actions.push(Action::ShowKeymapCheatsheet);
    actions.push(Action::ApplyTheme("wezterm".into()));
    actions.push(Action::OpenSshPane("alice@example.com".into()));
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
        // UI chrome
        Action::ToggleTabBar,
        // Window
        Action::NewWindow,
        Action::ToggleFullscreen,
        // Search / palette / editable config files
        Action::OpenSearch,
        Action::OpenCommandPalette,
        Action::EditConfigFile,
        Action::OpenKeymapFile,
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
        Action::RenameTab,
    ]
}

fn palette_accepts_keymap_action(action: &Action) -> bool {
    !matches!(action, Action::OpenSshPane(_) | Action::ShowKeymapCheatsheet)
}

/// Coverage assertion: every variant kind from
/// [`ALL_VARIANT_KINDS`] is represented by at least one entry in
/// [`all_actions`]. Lives here (not in the test crate) so the public
/// invariant is documented next to the data.
#[must_use]
pub fn covers_every_variant_kind() -> bool {
    use crate::command_label::variant_kind;
    let universe = all_actions();
    ALL_VARIANT_KINDS.iter().all(|kind| universe.iter().any(|a| variant_kind(a) == *kind))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sonicterm_cfg::keymap::{ActionWrapper, Binding, Keymap, Meta};

    #[test]
    fn palette_defaults_do_not_expose_placeholder_parameter_actions() {
        let actions = palette_actions();
        assert!(!actions.iter().any(|a| matches!(a, Action::ApplyTheme(_))));
        assert!(!actions.iter().any(|a| matches!(a, Action::OpenSshPane(_))));
        assert!(!actions.iter().any(|a| matches!(a, Action::ShowKeymapCheatsheet)));
        assert!(actions.iter().any(|a| matches!(a, Action::OpenCommandPalette)));
        assert!(actions
            .iter()
            .any(|a| { matches!(a, Action::ResizePane { dir: Direction::Left, amount: 5 }) }));
        assert!(covers_every_variant_kind());
    }

    #[test]
    fn palette_imports_concrete_keymap_theme_actions_and_shortcuts() {
        let keymap = Keymap {
            meta: Meta { name: "test".into(), version: "1.0".into() },
            bindings: vec![
                Binding {
                    keys: "super+shift+y".into(),
                    action: ActionWrapper(Action::ApplyTheme("wezterm".into())),
                },
                Binding {
                    keys: "super+shift+s".into(),
                    action: ActionWrapper(Action::OpenSshPane("alice@example.com".into())),
                },
                Binding {
                    keys: "super+shift+?".into(),
                    action: ActionWrapper(Action::ShowKeymapCheatsheet),
                },
            ],
        };
        let mut palette = CommandPalette::new();
        palette.set_keymap(&keymap);
        let visible = palette.visible();
        let theme_idx = visible
            .iter()
            .position(|a| matches!(a, Action::ApplyTheme(name) if name == "wezterm"))
            .expect("concrete keymap theme action should be visible");
        assert_eq!(palette.shortcut_hint_for_visible_index(theme_idx), Some("⌘⇧Y"));
        assert!(!visible.iter().any(|a| matches!(a, Action::OpenSshPane(_))));
        assert!(!visible.iter().any(|a| matches!(a, Action::ShowKeymapCheatsheet)));
    }

    #[test]
    fn palette_hides_activate_tab_entries_beyond_current_tab_count() {
        let keymap = Keymap {
            meta: Meta { name: "test".into(), version: "1.0".into() },
            bindings: vec![
                Binding { keys: "super+1".into(), action: ActionWrapper(Action::ActivateTab(0)) },
                Binding { keys: "super+2".into(), action: ActionWrapper(Action::ActivateTab(1)) },
                Binding { keys: "super+3".into(), action: ActionWrapper(Action::ActivateTab(2)) },
            ],
        };
        let mut palette = CommandPalette::new();
        palette.set_keymap(&keymap);
        palette.set_tab_count(2);
        let visible = palette.visible();
        assert!(visible.iter().any(|a| matches!(a, Action::ActivateTab(0))));
        assert!(visible.iter().any(|a| matches!(a, Action::ActivateTab(1))));
        assert!(!visible.iter().any(|a| matches!(a, Action::ActivateTab(2))));
    }
}
