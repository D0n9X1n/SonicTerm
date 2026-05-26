//! Command palette (Cmd+Shift+P). Pure-data state holder.
//!
//! The palette is a fuzzy-searchable list of every bindable
//! [`sonic_core::keymap::Action`]. The keyboard-event handler in
//! [`crate::app`] routes printable characters, arrow keys, Enter and Esc
//! into this state instead of forwarding them to the active pty when
//! [`CommandPalette::is_open`] returns `true`. On Enter the dispatcher
//! reads [`CommandPalette::current`] and runs that action through
//! `App::run_action`.
//!
//! Visual rendering is deferred to a follow-up (see
//! `App::draw_command_palette_overlay`); this module is state only.
//!
//! Filtering is a simple subsequence (a.k.a. "fzf-lite") match on the
//! lowercased display name of each action. Empty query matches everything
//! in the canonical order returned by [`all_actions`].

use sonic_core::keymap::{Action, Direction, ScrollAction};

/// State for the command palette overlay. Owned by `App`.
#[derive(Debug, Clone)]
pub struct CommandPalette {
    open: bool,
    query: String,
    /// Full universe of actions, in canonical order.
    all: Vec<Action>,
    /// Filtered view — indices into `all` matched by the current query,
    /// or all indices when the query is empty.
    items: Vec<usize>,
    selected: usize,
}

impl Default for CommandPalette {
    fn default() -> Self {
        Self::new()
    }
}

impl CommandPalette {
    pub fn new() -> Self {
        let all = all_actions();
        let items = (0..all.len()).collect();
        Self { open: false, query: String::new(), all, items, selected: 0 }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    /// Visible action list (filtered). Display order is what the renderer
    /// should show.
    pub fn visible(&self) -> Vec<&Action> {
        self.items.iter().filter_map(|&i| self.all.get(i)).collect()
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
        self.query.clear();
        self.selected = 0;
        self.refilter();
    }

    pub fn close(&mut self) {
        self.open = false;
        self.query.clear();
        self.selected = 0;
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
        self.refilter();
    }

    pub fn input_char(&mut self, ch: char) {
        self.query.push(ch);
        self.selected = 0;
        self.refilter();
    }

    pub fn backspace(&mut self) {
        if self.query.pop().is_some() {
            self.selected = 0;
            self.refilter();
        }
    }

    pub fn move_selection_down(&mut self) {
        if self.items.is_empty() {
            self.selected = 0;
            return;
        }
        self.selected = (self.selected + 1) % self.items.len();
    }

    pub fn move_selection_up(&mut self) {
        if self.items.is_empty() {
            self.selected = 0;
            return;
        }
        self.selected = if self.selected == 0 { self.items.len() - 1 } else { self.selected - 1 };
    }

    /// The currently highlighted action, if any.
    pub fn current(&self) -> Option<&Action> {
        self.items.get(self.selected).and_then(|&i| self.all.get(i))
    }

    fn refilter(&mut self) {
        let q = self.query.to_lowercase();
        if q.is_empty() {
            self.items = (0..self.all.len()).collect();
        } else {
            self.items = self
                .all
                .iter()
                .enumerate()
                .filter(|(_, a)| subsequence_match(&action_display_name(a).to_lowercase(), &q))
                .map(|(i, _)| i)
                .collect();
        }
        if self.selected >= self.items.len() {
            self.selected = 0;
        }
    }
}

/// Subsequence (a.k.a. "fzf-lite") match: every character of `needle`
/// appears in `haystack` in order, but not necessarily contiguously.
fn subsequence_match(haystack: &str, needle: &str) -> bool {
    let mut chars = haystack.chars();
    'outer: for nc in needle.chars() {
        for hc in chars.by_ref() {
            if hc == nc {
                continue 'outer;
            }
        }
        return false;
    }
    true
}

/// Human-readable name for an action, used both for fuzzy matching and
/// for the palette renderer.
pub fn action_display_name(a: &Action) -> String {
    match a {
        Action::NewTab => "NewTab".into(),
        Action::CloseTab => "CloseTab".into(),
        Action::NextTab => "NextTab".into(),
        Action::PrevTab => "PrevTab".into(),
        Action::ActivateTab(i) => format!("ActivateTab({i})"),
        Action::ActivateLastTab => "ActivateLastTab".into(),
        Action::SplitRight => "SplitRight".into(),
        Action::SplitDown => "SplitDown".into(),
        Action::ClosePane => "ClosePane".into(),
        Action::FocusPane(d) => format!("FocusPane({})", dir_name(*d)),
        Action::ResizePane { dir, amount } => {
            format!("ResizePane({}, {amount})", dir_name(*dir))
        }
        Action::CopyToClipboard => "CopyToClipboard".into(),
        Action::PasteFromClipboard => "PasteFromClipboard".into(),
        Action::IncreaseFontSize => "IncreaseFontSize".into(),
        Action::DecreaseFontSize => "DecreaseFontSize".into(),
        Action::ResetFontSize => "ResetFontSize".into(),
        Action::NewWindow => "NewWindow".into(),
        Action::ToggleFullscreen => "ToggleFullscreen".into(),
        Action::OpenSearch => "OpenSearch".into(),
        Action::OpenCommandPalette => "OpenCommandPalette".into(),
        Action::OpenPreferences => "OpenPreferences".into(),
        Action::Scroll(s) => format!("Scroll({})", scroll_name(*s)),
        Action::ScrollToPrevPrompt => "ScrollToPrevPrompt".into(),
        Action::ScrollToNextPrompt => "ScrollToNextPrompt".into(),
        Action::ReloadConfig => "ReloadConfig".into(),
        Action::OpenSshPane(t) => format!("OpenSshPane({t})"),
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

/// Canonical list of every bindable action, in the order the palette
/// should present them when no query is entered. Keep grouped by feature
/// area for readability.
pub fn all_actions() -> Vec<Action> {
    vec![
        // Tabs
        Action::NewTab,
        Action::CloseTab,
        Action::NextTab,
        Action::PrevTab,
        Action::ActivateLastTab,
        // Splits
        Action::SplitRight,
        Action::SplitDown,
        Action::ClosePane,
        Action::FocusPane(Direction::Left),
        Action::FocusPane(Direction::Right),
        Action::FocusPane(Direction::Up),
        Action::FocusPane(Direction::Down),
        // Clipboard
        Action::CopyToClipboard,
        Action::PasteFromClipboard,
        // Font
        Action::IncreaseFontSize,
        Action::DecreaseFontSize,
        Action::ResetFontSize,
        // Window
        Action::NewWindow,
        Action::ToggleFullscreen,
        // Search / palette / prefs
        Action::OpenSearch,
        Action::OpenCommandPalette,
        Action::OpenPreferences,
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
    ]
}
