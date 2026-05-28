//! Command palette (Cmd+Shift+P). Pure-data state holder.
//!
//! The palette is a fuzzy-searchable list of every bindable
//! [`sonic_cfg::keymap::Action`]. The keyboard-event handler in
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
use sonic_cfg::keymap::{Action, Direction, ScrollAction};

use crate::command_label::{search_haystack, ALL_VARIANT_KINDS};

/// State for the command palette overlay. Owned by `App`.
#[derive(Debug, Clone)]
pub struct CommandPalette {
    open: bool,
    query: String,
    /// Full universe of actions, in canonical order.
    all: Vec<Action>,
    /// Filtered view — indices into `all` matched by the current query,
    /// or all indices when the query is empty. Order is descending
    /// fuzzy-score, with canonical-order tiebreak.
    items: Vec<usize>,
    selected: usize,
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
        let all = all_actions();
        let items = (0..all.len()).collect();
        Self {
            open: false,
            query: String::new(),
            all,
            items,
            selected: 0,
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
        self.scroll_offset = 0;
        self.refilter();
    }

    pub fn close(&mut self) {
        self.open = false;
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
        self.refilter();
    }

    pub fn input_char(&mut self, ch: char) {
        self.query.push(ch);
        self.selected = 0;
        self.scroll_offset = 0;
        self.refilter();
    }

    pub fn backspace(&mut self) {
        if self.query.pop().is_some() {
            self.selected = 0;
            self.scroll_offset = 0;
            self.refilter();
        }
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
        self.items.get(self.selected).and_then(|&i| self.all.get(i))
    }

    /// Fuzzy-match `query` against the human label of each candidate
    /// action; sort hits descending by nucleo score with canonical-
    /// order tiebreak. Empty query is canonical order, full universe.
    fn refilter(&mut self) {
        if self.query.is_empty() {
            self.items = (0..self.all.len()).collect();
        } else {
            let mut matcher = Matcher::new(Config::DEFAULT);
            let pattern = Pattern::parse(&self.query, CaseMatching::Ignore, Normalization::Smart);
            let mut scratch: Vec<char> = Vec::new();
            let mut scored: Vec<(usize, u32)> = self
                .all
                .iter()
                .enumerate()
                .filter_map(|(i, a)| {
                    scratch.clear();
                    let label = search_haystack(a);
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
}

/// Backwards-compatible display name. The palette overlay rendering
/// now prefers the friendlier [`crate::command_label::label`], but
/// existing callers/tests that asked for `"NewTab"` (PascalCase
/// variant name) still get that here.
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
        Action::PasteFromClipboard => "PasteFromClipboard".into(),
        Action::IncreaseFontSize => "IncreaseFontSize".into(),
        Action::DecreaseFontSize => "DecreaseFontSize".into(),
        Action::ResetFontSize => "ResetFontSize".into(),
        Action::NewWindow => "NewWindow".into(),
        Action::ToggleFullscreen => "ToggleFullscreen".into(),
        Action::OpenSearch => "OpenSearch".into(),
        Action::OpenCommandPalette => "OpenCommandPalette".into(),
        Action::ShowKeymapCheatsheet => "ShowKeymapCheatsheet".into(),
        Action::OpenPreferences => "OpenPreferences".into(),
        Action::Scroll(s) => format!("Scroll({})", scroll_name(*s)),
        Action::ScrollToPrevPrompt => "ScrollToPrevPrompt".into(),
        Action::ScrollToNextPrompt => "ScrollToNextPrompt".into(),
        Action::ReloadConfig => "ReloadConfig".into(),
        Action::OpenSshPane(t) => format!("OpenSshPane({t})"),
        Action::ApplyTheme(name) => format!("ApplyTheme({name})"),
        Action::ToggleTabBar => "ToggleTabBar".into(),
    }
}

fn broadcast_scope_name(scope: sonic_cfg::keymap::BroadcastScope) -> &'static str {
    match scope {
        sonic_cfg::keymap::BroadcastScope::Tab => "Tab",
        sonic_cfg::keymap::BroadcastScope::AllTabs => "AllTabs",
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
/// should present them when no query is entered. Keep grouped by
/// feature area for readability. **Every variant kind in
/// [`sonic_cfg::keymap::Action`] must appear at least once**; the
/// `palette_lists_every_action_variant` integration test asserts this.
pub fn all_actions() -> Vec<Action> {
    vec![
        // Tabs
        Action::NewTab,
        Action::CloseTab,
        Action::NextTab,
        Action::PrevTab,
        Action::ActivateLastTab,
        Action::ActivateTab(1),
        // Splits
        Action::SplitRight,
        Action::SplitDown,
        Action::ClosePane,
        Action::TogglePaneZoom,
        Action::ToggleBroadcast { scope: sonic_cfg::keymap::BroadcastScope::Tab },
        Action::ToggleBroadcast { scope: sonic_cfg::keymap::BroadcastScope::AllTabs },
        Action::FocusPane(Direction::Left),
        Action::FocusPane(Direction::Right),
        Action::FocusPane(Direction::Up),
        Action::FocusPane(Direction::Down),
        Action::ResizePaneLeft,
        Action::ResizePaneRight,
        Action::ResizePaneUp,
        Action::ResizePaneDown,
        Action::ResizePane { dir: Direction::Left, amount: 1 },
        Action::ResizePane { dir: Direction::Right, amount: 1 },
        Action::ResizePane { dir: Direction::Up, amount: 1 },
        Action::ResizePane { dir: Direction::Down, amount: 1 },
        // Clipboard
        Action::CopyToClipboard,
        Action::EnterCopyMode,
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
        // Search / palette / prefs
        Action::OpenSearch,
        Action::OpenCommandPalette,
        Action::ShowKeymapCheatsheet,
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
        // Theme (one representative; full theme list lives elsewhere
        // and is wired via menubar). Listing one entry keeps the
        // variant kind present in the palette universe.
        Action::ApplyTheme("default".into()),
        // SSH (one representative; the user fills in target via a
        // future prompt). Variant kept so the palette is exhaustive
        // even when ssh is disabled at build time.
        Action::OpenSshPane("user@host".into()),
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
    ALL_VARIANT_KINDS.iter().all(|kind| universe.iter().any(|a| variant_kind(a) == *kind))
}
