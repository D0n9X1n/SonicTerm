//! Command palette (Cmd+Shift+P). Pure-data state holder.
//!
//! The palette is a fuzzy-searchable list of runnable
//! [`sonicterm_cfg::keymap::Action`] values. The app's keyboard-event handler
//! routes printable characters, arrow keys, Enter and Esc
//! into this state instead of forwarding them to the active pty when
//! [`CommandPalette::is_open`] returns `true`. On Enter the dispatcher
//! reads [`CommandPalette::current`] and runs that action.
//!
//! Filtering is a VSCode-style fuzzy match using
//! [`nucleo_matcher`]: each candidate label gets a score, results are
//! sorted descending by score, and ties fall back to the canonical
//! order returned by [`all_actions`]. Empty query groups commands by category
//! with their relative canonical order preserved. Subsequence matching is the underlying ranker,
//! so substring runs score above scattered matches.

use nucleo_matcher::{
    pattern::{CaseMatching, Normalization, Pattern},
    Config, Matcher, Utf32Str,
};
use sonicterm_cfg::keymap::{Action, Direction, Keymap, ScrollAction};

use crate::command_label::{
    descriptor, disabled_reason, keybinding_hint, label, localized_label,
    localized_search_haystack, search_haystack, CommandCategory, CommandContext, DisabledReason,
    ALL_VARIANT_KINDS,
};
use crate::i18n::I18n;
use crate::tabs::{TabBar, TabId};
use crate::text_edit::{apply_edit, TextEdit};
use std::hash::{Hash, Hasher};

const NO_SELECTION: usize = usize::MAX;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandPaletteMode {
    Commands,
    RenameTab,
    TabColor,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabColorChoice {
    pub name: String,
    pub hex: Option<String>,
}

/// A palette command or a live tab target whose identity is independent of its display position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteEntry {
    /// An existing keymap action executed by App.
    Command(Action),
    /// A tab in the attached terminal window.
    Tab {
        /// Process-unique runtime identity, revalidated before activation.
        id: TabId,
        /// Current literal title.
        title: String,
        /// Zero-based current position, used only for the display label.
        position: usize,
    },
}

impl PaletteEntry {
    /// Compare executable identity without treating tab titles or positions as targets.
    pub fn same_identity(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Command(left), Self::Command(right)) => left == right,
            (Self::Tab { id: left, .. }, Self::Tab { id: right, .. }) => left == right,
            _ => false,
        }
    }

    fn category(&self) -> CommandCategory {
        match self {
            Self::Command(action) => descriptor(action).category,
            Self::Tab { .. } => CommandCategory::Tabs,
        }
    }

    fn disabled_reason(&self, context: &CommandContext) -> Option<DisabledReason> {
        match self {
            Self::Command(action) => disabled_reason(action, context),
            Self::Tab { .. } if !context.window_available => Some(DisabledReason::NoWindow),
            Self::Tab { .. } if context.tab_count == 0 => Some(DisabledReason::NoTab),
            Self::Tab { .. } => None,
        }
    }

    fn presentation(&self, i18n: Option<&I18n>, hint: Option<String>) -> CommandPresentation {
        let (label, mut search) = match self {
            Self::Command(action) => i18n.map_or_else(
                || (label(action), search_haystack(action)),
                |i18n| (localized_label(action, i18n), localized_search_haystack(action, i18n)),
            ),
            Self::Tab { title, position, .. } => {
                let number = (position + 1).to_string();
                let english = format!("Go to Tab {number}: {title}");
                let label = i18n
                    .and_then(|i18n| {
                        i18n.try_t_args(
                            "palette-go-to-tab",
                            Some(&[("number", &number), ("title", title)]),
                        )
                    })
                    .unwrap_or_else(|| english.clone());
                let search = if label == english {
                    format!("{english} switch tab")
                } else {
                    // When: `label` is translated, English Go to Tab queries must still discover the target.
                    format!("{label} {english} switch tab")
                };
                (label, search)
            }
        };
        if let Some(hint) = &hint {
            search.push(' ');
            search.push_str(hint);
        }
        CommandPresentation { label, search, hint }
    }
}

#[derive(Debug, Clone, Hash)]
struct CommandPresentation {
    label: String,
    search: String,
    hint: Option<String>,
}

// Resolve Fluent once; insert literal values into cached phrase slots during layout.
const TEXT_SLOT: &str = "\u{fdd0}";

#[derive(Debug, Clone, Hash)]
struct TextSlot {
    before: String,
    after: String,
}

impl TextSlot {
    fn new(message: String) -> Self {
        let (before, after) = message
            .split_once(TEXT_SLOT)
            .expect("embedded palette phrase retains its argument slot");
        Self { before: before.to_string(), after: after.to_string() }
    }

    fn render(&self, value: &str) -> String {
        format!("{}{value}{}", self.before, self.after)
    }
}

#[derive(Debug, Clone, Hash)]
pub(crate) struct PaletteText {
    pub(crate) search_placeholder: String,
    pub(crate) tabs_placeholder: String,
    pub(crate) tabs_empty: String,
    pub(crate) tabs_hint: String,
    pub(crate) tabs_footer: String,
    pub(crate) rename_placeholder: String,
    pub(crate) no_matches: String,
    pub(crate) empty_hint: String,
    pub(crate) rename_footer: String,
    pub(crate) color_footer: String,
    command_footer_one: TextSlot,
    command_footer_other: TextSlot,
    color_title: TextSlot,
    categories: [String; 7],
    disabled_reasons: [String; 7],
}

impl PaletteText {
    fn new(i18n: Option<&I18n>) -> Self {
        let text = |key, fallback: &str| {
            i18n.and_then(|i18n| i18n.try_t_args(key, None)).unwrap_or_else(|| fallback.to_string())
        };
        let footer = |kind, noun| {
            let args = [("count", TEXT_SLOT), ("count-kind", kind)];
            TextSlot::new(
                i18n.and_then(|i18n| i18n.try_t_args("palette-command-footer", Some(&args)))
                    .unwrap_or_else(|| {
                        format!("{TEXT_SLOT} {noun} · ↑↓ navigate · ↵ run · esc close")
                    }),
            )
        };
        let color_title = TextSlot::new(
            i18n.and_then(|i18n| {
                i18n.try_t_args("palette-color-title", Some(&[("title", TEXT_SLOT)]))
            })
            .unwrap_or_else(|| format!("Color for {TEXT_SLOT}")),
        );
        Self {
            search_placeholder: text(
                "palette-search-placeholder",
                "Search commands, settings, shortcuts…",
            ),
            tabs_placeholder: text("palette-tabs-placeholder", "Search tabs…"),
            tabs_empty: text("palette-tabs-empty", "No tabs found"),
            tabs_hint: text("palette-tabs-hint", "Search a tab title or position"),
            tabs_footer: text(
                "palette-tabs-footer",
                "All tabs · ↑↓ navigate · ↵ switch · esc close",
            ),
            rename_placeholder: text("palette-rename-placeholder", "New tab title…"),
            no_matches: text("palette-no-matches", "No commands found"),
            empty_hint: text("palette-empty-hint", "Try settings, split, font, shortcut"),
            rename_footer: text("palette-rename-footer", "↵ rename · esc cancel"),
            color_footer: text("palette-color-footer", "↑↓ choose color · ↵ apply · esc cancel"),
            command_footer_one: footer("one", "command"),
            command_footer_other: footer("other", "commands"),
            color_title,
            categories: CommandCategory::ALL.map(|category| {
                let (key, fallback) = category.message();
                text(key, fallback)
            }),
            disabled_reasons: DisabledReason::ALL.map(|reason| {
                let (key, fallback) = reason.message();
                text(key, fallback)
            }),
        }
    }

    /// Format the current result count through the locale's cached whole phrase.
    pub(crate) fn command_footer(&self, count: usize) -> String {
        let phrase = if count == 1 { &self.command_footer_one } else { &self.command_footer_other };
        phrase.render(&count.to_string())
    }

    /// Preserve literal tab titles inside the locale's phrase and append the UI caret.
    pub(crate) fn color_title(&self, title: &str) -> String {
        format!("{}▏", self.color_title.render(title))
    }
}

/// State for the command palette overlay. Owned by `App`.
#[derive(Debug, Clone)]
pub struct CommandPalette {
    open: bool,
    mode: CommandPaletteMode,
    tabs_only: bool,
    query: String,
    cursor: usize,
    /// Canonical commands followed by the attached window's live tab targets.
    all: Vec<PaletteEntry>,
    presentation: Vec<CommandPresentation>,
    text: PaletteText,
    presentation_hash: u64,
    /// Command indices into `all`, or color-choice indices in TabColor mode.
    items: Vec<usize>,
    selected: usize,
    context: CommandContext,
    /// First visible item index in the rendered viewport. Maintained by
    /// [`Self::ensure_selected_in_view`] so that arrow-key navigation
    /// keeps the highlighted row inside the modal even when the
    /// filtered list is longer than `visible_rows`.
    scroll_offset: usize,
    /// Cached count of rows the renderer can actually display, set via
    /// [`Self::set_visible_rows`]. Zero means "unconstrained" — used by
    /// tests that don't know the modal size yet.
    visible_rows: usize,
    tab_color_title: String,
    tab_color_choices: Vec<TabColorChoice>,
}

impl Default for CommandPalette {
    fn default() -> Self {
        Self::new()
    }
}

impl CommandPalette {
    /// Build a closed palette holding the canonical action list.
    pub fn new() -> Self {
        let all: Vec<_> = palette_actions().into_iter().map(PaletteEntry::Command).collect();
        let presentation: Vec<_> = all.iter().map(|entry| entry.presentation(None, None)).collect();
        let text = PaletteText::new(None);
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        presentation.hash(&mut hash);
        text.hash(&mut hash);
        let context = CommandContext::default();
        context.hash(&mut hash);
        let presentation_hash = hash.finish();
        let mut items: Vec<_> = (0..all.len()).collect();
        items.sort_by_key(|&index| all[index].category());
        Self {
            open: false,
            mode: CommandPaletteMode::Commands,
            tabs_only: false,
            query: String::new(),
            cursor: 0,
            all,
            presentation,
            text,
            presentation_hash,
            items,
            selected: 0,
            context,
            scroll_offset: 0,
            visible_rows: 0,
            tab_color_title: String::new(),
            tab_color_choices: Vec::new(),
        }
    }

    /// Report whether the overlay is showing and should absorb key events.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Current query text, as typed.
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Byte offset of the text cursor within [`Self::query`].
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Which input the overlay is collecting: commands, a tab name, or a colour.
    pub fn mode(&self) -> CommandPaletteMode {
        self.mode
    }

    /// Index of the highlighted row within the filtered view.
    pub fn selected(&self) -> usize {
        self.selected
    }

    /// Filtered command and tab entries in display order.
    pub fn visible(&self) -> Vec<&PaletteEntry> {
        if self.mode != CommandPaletteMode::Commands {
            // When: `mode` is not `Commands`, the overlay lists tab names or colours, not actions.
            return Vec::new();
        }
        self.items.iter().filter_map(|&i| self.all.get(i)).collect()
    }

    /// Localized command label in the filtered display order.
    pub fn label_for_visible_index(&self, visible_index: usize) -> Option<&str> {
        self.presentation_for_visible_index(visible_index).map(|entry| entry.label.as_str())
    }

    /// Disabled reason for a command row, while keeping it available for search and inspection.
    pub fn disabled_reason_for_visible_index(
        &self,
        visible_index: usize,
    ) -> Option<DisabledReason> {
        if self.mode != CommandPaletteMode::Commands {
            // When: `mode` uses title or color state, no command requirement applies to the row.
            return None;
        }
        self.all.get(*self.items.get(visible_index)?)?.disabled_reason(&self.context)
    }

    /// Localized category and availability text for a command row.
    pub fn detail_for_visible_index(&self, visible_index: usize) -> Option<String> {
        if self.mode != CommandPaletteMode::Commands {
            // When: `mode` is not Commands, command category details do not describe these rows.
            return None;
        }
        let entry = self.all.get(*self.items.get(visible_index)?)?;
        let category = &self.text.categories[entry.category() as usize];
        Some(match entry.disabled_reason(&self.context) {
            Some(reason) => format!("{category} · {}", self.text.disabled_reasons[reason as usize]),
            None => category.clone(),
        })
    }

    /// Keybinding hint for a row of [`Self::visible`], in the same display order.
    pub fn shortcut_hint_for_visible_index(&self, visible_index: usize) -> Option<&str> {
        self.presentation_for_visible_index(visible_index)?.hint.as_deref()
    }

    fn presentation_for_visible_index(&self, visible_index: usize) -> Option<&CommandPresentation> {
        if self.mode != CommandPaletteMode::Commands {
            // When: `mode` is not Commands, `items` does not index command presentation records.
            return None;
        }
        self.presentation.get(*self.items.get(visible_index)?)
    }

    /// Cached command-text identity for retained-frame invalidation without per-frame translation.
    pub fn presentation_hash(&self) -> u64 {
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        self.presentation_hash.hash(&mut hash);
        self.tabs_only.hash(&mut hash);
        hash.finish()
    }

    /// Cached locale text shared by all palette layout modes.
    pub(crate) fn text(&self) -> &PaletteText {
        &self.text
    }

    /// Number of rows in the filtered view.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Report whether the query matched nothing.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Open the palette and reset to a clean state.
    pub fn open(&mut self) {
        self.open = true;
        self.mode = CommandPaletteMode::Commands;
        self.tabs_only = false;
        self.query.clear();
        self.cursor = 0;
        self.selected = 0;
        self.scroll_offset = 0;
        self.refilter();
    }

    /// Open the same palette restricted to live tabs in its attached window.
    pub fn open_tabs(&mut self) {
        self.open();
        self.tabs_only = true;
        self.refilter();
    }

    /// Whether the command input is showing only live tab targets.
    pub fn tabs_only(&self) -> bool {
        self.tabs_only
    }

    /// Close the palette and clear the query so the next open starts clean.
    pub fn close(&mut self) {
        self.open = false;
        self.mode = CommandPaletteMode::Commands;
        self.tabs_only = false;
        self.query.clear();
        self.cursor = 0;
        self.selected = 0;
        self.scroll_offset = 0;
        self.refilter();
    }

    /// Toggle open/close. Returns the new open state.
    pub fn toggle(&mut self) -> bool {
        if self.open {
            self.close();
        } else {
            // When: `open` is false, the same shortcut reopens the overlay from a clean query.
            self.open();
        }
        self.open
    }

    /// Replace the query wholesale and re-filter, putting the cursor at the end.
    pub fn set_query(&mut self, q: impl Into<String>) {
        self.query = q.into();
        self.cursor = self.query.len();
        self.selected = 0;
        self.scroll_offset = 0;
        if self.mode == CommandPaletteMode::Commands {
            self.refilter();
        }
    }

    /// Rebuild command bindings and localized text while retaining the current tab-target inventory.
    pub fn set_keymap(&mut self, keymap: &Keymap, i18n: &I18n) {
        let selected = self.highlighted().cloned();
        let targets: Vec<_> =
            self.all.drain(..).filter(|entry| matches!(entry, PaletteEntry::Tab { .. })).collect();
        self.all = palette_actions().into_iter().map(PaletteEntry::Command).collect();
        for binding in &keymap.bindings {
            let action = &binding.action.0;
            if palette_accepts_keymap_action(action)
                && !self.all.iter().any(
                    |entry| matches!(entry, PaletteEntry::Command(existing) if existing == action),
                )
            {
                self.all.push(PaletteEntry::Command(action.clone()));
            }
        }
        self.all.extend(targets);
        self.presentation = self
            .all
            .iter()
            .map(|entry| {
                let hint = match entry {
                    PaletteEntry::Command(action) => keybinding_hint(keymap, action),
                    PaletteEntry::Tab { .. } => None,
                };
                entry.presentation(Some(i18n), hint)
            })
            .collect();
        self.text = PaletteText::new(Some(i18n));
        self.refresh_identity();
        self.refilter_preserving_entry(selected);
    }

    /// Refresh translated text without changing literal targets, input, or noncommand picker selection.
    pub fn set_locale(&mut self, i18n: &I18n) {
        let selected = self.highlighted().cloned();
        for (entry, presentation) in self.all.iter().zip(&mut self.presentation) {
            *presentation = entry.presentation(Some(i18n), presentation.hint.take());
        }
        self.text = PaletteText::new(Some(i18n));
        self.refresh_identity();
        self.refilter_preserving_entry(selected);
    }

    /// Refresh only the attached window's live tab targets, preserving selection by TabId.
    pub fn set_tabs(&mut self, tabs: &TabBar, i18n: &I18n) {
        let command_count = self
            .all
            .iter()
            .position(|entry| matches!(entry, PaletteEntry::Tab { .. }))
            .unwrap_or(self.all.len());
        let unchanged = self.all.len() - command_count == tabs.len()
            && self.all[command_count..].iter().zip(tabs.tabs()).enumerate().all(|(position, (entry, tab))| {
                matches!(entry, PaletteEntry::Tab { id, title, position: previous } if *id == tab.id && title == &tab.title && *previous == position)
            });
        if unchanged {
            // When: tab identity, title, and position are unchanged, retain cached strings and selection.
            return;
        }
        let selected = self.highlighted().cloned();
        for (position, tab) in tabs.tabs().iter().enumerate() {
            let index = command_count + position;
            if matches!(self.all.get(index), Some(PaletteEntry::Tab { id, title, position: previous }) if *id == tab.id && title == &tab.title && *previous == position)
            {
                // When: the existing tab entry matches, retain its translated presentation and allocated strings.
                continue;
            }
            let entry = PaletteEntry::Tab { id: tab.id, title: tab.title.clone(), position };
            let presentation = entry.presentation(Some(i18n), None);
            if index < self.all.len() {
                self.all[index] = entry;
                self.presentation[index] = presentation;
            } else {
                // When: index extends the inventory, append one new tab and its matching presentation.
                self.all.push(entry);
                self.presentation.push(presentation);
            }
        }
        self.all.truncate(command_count + tabs.len());
        self.presentation.truncate(command_count + tabs.len());
        self.refresh_identity();
        self.refilter_preserving_entry(selected);
    }

    fn refresh_identity(&mut self) {
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        self.presentation.hash(&mut hash);
        self.text.hash(&mut hash);
        self.context.hash(&mut hash);
        for entry in &self.all {
            if let PaletteEntry::Tab { id, .. } = entry {
                id.hash(&mut hash);
            }
        }
        self.presentation_hash = hash.finish();
    }

    fn refilter_preserving_entry(&mut self, selected: Option<PaletteEntry>) {
        if self.mode != CommandPaletteMode::Commands {
            // When: `mode` uses title or color state, refresh text but preserve its separate `items` indices.
            return;
        }
        let scroll = self.scroll_offset;
        let keep_absence =
            self.selected == NO_SELECTION || matches!(selected, Some(PaletteEntry::Tab { .. }));
        self.refilter();
        self.selected = selected
            .as_ref()
            .and_then(|entry| {
                self.items.iter().position(|&index| self.all[index].same_identity(entry))
            })
            .unwrap_or(if keep_absence { NO_SELECTION } else { 0 });
        self.scroll_offset = if self.items.is_empty() { 0 } else { scroll };
        self.ensure_selected_in_view();
    }

    /// Replace attached-window facts without changing the highlighted action or translating text.
    pub fn set_context(&mut self, context: CommandContext) {
        if self.context == context {
            // When: `context` is unchanged, retain the existing selection and presentation identity.
            return;
        }
        let selected = self.highlighted().cloned();
        self.context = context;
        self.refresh_identity();
        self.refilter_preserving_entry(selected);
    }

    /// Insert a typed character at the cursor and re-filter.
    pub fn input_char(&mut self, ch: char) {
        self.query.insert(self.cursor, ch);
        self.cursor += ch.len_utf8();
        self.selected = 0;
        self.scroll_offset = 0;
        if self.mode == CommandPaletteMode::Commands {
            self.refilter();
        }
    }

    /// Apply a cursor move or deletion, re-filtering only when the text changed.
    pub fn apply_text_edit(&mut self, edit: TextEdit) {
        let outcome = apply_edit(&mut self.query, self.cursor, edit);
        self.cursor = outcome.cursor;
        if outcome.changed {
            self.selected = 0;
            self.scroll_offset = 0;
            if self.mode == CommandPaletteMode::Commands {
                self.refilter();
            }
        }
    }

    /// Delete the character before the cursor.
    pub fn backspace(&mut self) {
        self.apply_text_edit(TextEdit::DeleteBackward);
    }

    /// Switch to tab-rename mode, seeding the field with the current title.
    pub fn start_rename_tab(&mut self, title_body: impl Into<String>) {
        self.open = true;
        self.mode = CommandPaletteMode::RenameTab;
        self.query = title_body.into();
        self.cursor = self.query.len();
        self.items.clear();
        self.selected = 0;
        self.scroll_offset = 0;
    }

    /// Switch to tab-colour mode, listing `choices` for the named tab.
    pub fn start_tab_color_picker(
        &mut self,
        tab_title: impl Into<String>,
        choices: Vec<TabColorChoice>,
    ) {
        self.open = true;
        self.mode = CommandPaletteMode::TabColor;
        self.query.clear();
        self.cursor = 0;
        self.items = (0..choices.len()).collect();
        self.selected = 0;
        self.scroll_offset = 0;
        self.tab_color_title = tab_title.into();
        self.tab_color_choices = choices;
    }

    /// Title of the tab the colour picker is editing.
    pub fn tab_color_title(&self) -> &str {
        &self.tab_color_title
    }

    /// Colour choices offered in tab-colour mode, in display order.
    pub fn tab_color_choices(&self) -> &[TabColorChoice] {
        &self.tab_color_choices
    }

    /// Highlighted colour choice, if the selection still indexes the list.
    pub fn selected_tab_color(&self) -> Option<&TabColorChoice> {
        self.tab_color_choices.get(self.selected)
    }

    /// Move the text cursor one character left.
    pub fn move_cursor_left(&mut self) {
        self.apply_text_edit(TextEdit::MoveBackward);
    }

    /// Move the text cursor one character right.
    pub fn move_cursor_right(&mut self) {
        self.apply_text_edit(TextEdit::MoveForward);
    }

    /// Move the text cursor to the start of the query.
    pub fn move_cursor_home(&mut self) {
        self.apply_text_edit(TextEdit::MoveStart);
    }

    /// Move the text cursor to the end of the query.
    pub fn move_cursor_end(&mut self) {
        self.apply_text_edit(TextEdit::MoveEnd);
    }

    /// Delete the character after the cursor.
    pub fn delete_forward(&mut self) {
        self.apply_text_edit(TextEdit::DeleteForward);
    }

    /// Highlight a filtered command row without executing it or permitting an unavailable command.
    pub fn select_visible_index(&mut self, index: usize) -> bool {
        if self.mode != CommandPaletteMode::Commands || index >= self.items.len() {
            // When: `index` is absent or `mode` is not Commands, preserve the existing picker selection.
            return false;
        }
        self.selected = index;
        self.ensure_selected_in_view();
        true
    }

    /// Highlight the next row, wrapping to the top past the last one.
    pub fn move_selection_down(&mut self) {
        if self.items.is_empty() {
            // When: `items` is empty, there is no row to highlight, so the view resets to the top.
            self.selected = 0;
            self.scroll_offset = 0;
            return;
        }
        self.selected = if self.selected == NO_SELECTION {
            0
        } else {
            // When: `selected` names a real item, advance and wrap without overflowing the absence sentinel.
            (self.selected + 1) % self.items.len()
        };
        self.ensure_selected_in_view();
    }

    /// Highlight the previous row, wrapping to the bottom past the first one.
    pub fn move_selection_up(&mut self) {
        if self.items.is_empty() {
            // When: `items` is empty, there is no row to highlight, so the view resets to the top.
            self.selected = 0;
            self.scroll_offset = 0;
            return;
        }
        self.selected = if self.selected == 0 || self.selected == NO_SELECTION {
            self.items.len() - 1
        } else {
            // When: `selected` is nonzero, stepping back stays inside the list without wrapping.
            self.selected - 1
        };
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

    /// Number of rows the renderer last reported it can display.
    pub fn visible_rows(&self) -> usize {
        self.visible_rows
    }

    /// Clamp `scroll_offset` so `selected` is always inside the
    /// `[scroll_offset, scroll_offset + visible_rows)` half-open window.
    /// When `visible_rows == 0` this is a no-op (no constraint known).
    fn ensure_selected_in_view(&mut self) {
        if self.visible_rows == 0 || self.items.is_empty() {
            // When: `visible_rows` is zero or `items` is empty, no window constrains the selection.
            return;
        }
        if self.selected == NO_SELECTION {
            // When: the selected tab disappeared, clamp the viewport without choosing another entry.
            self.scroll_offset =
                self.scroll_offset.min(self.items.len().saturating_sub(self.visible_rows));
            return;
        }
        if self.selected < self.scroll_offset {
            self.scroll_offset = self.selected;
        } else if self.selected >= self.scroll_offset + self.visible_rows {
            // When: `selected` sits past the window, scroll down so it becomes the last row.
            self.scroll_offset = self.selected + 1 - self.visible_rows;
        }
        // Don't leave a trailing gap of empty rows at the bottom when the
        // list shrinks under us (post-refilter).
        let max_off = self.items.len().saturating_sub(self.visible_rows);
        if self.scroll_offset > max_off {
            self.scroll_offset = max_off;
        }
    }

    /// The highlighted entry, including a disabled row whose identity must survive refresh.
    pub fn highlighted(&self) -> Option<&PaletteEntry> {
        if self.mode != CommandPaletteMode::Commands {
            // When: `mode` collects a title or color, `items` cannot select a command entry.
            return None;
        }
        self.items.get(self.selected).and_then(|&i| self.all.get(i))
    }

    /// The highlighted entry only when its attached-window requirements permit execution.
    pub fn current(&self) -> Option<&PaletteEntry> {
        self.highlighted().filter(|entry| entry.disabled_reason(&self.context).is_none())
    }

    /// Rank query matches with canonical ties; group the complete empty-query list by category.
    fn refilter(&mut self) {
        if self.query.is_empty() {
            self.items = (0..self.all.len())
                .filter(|&index| {
                    !self.tabs_only || matches!(self.all[index], PaletteEntry::Tab { .. })
                })
                .collect();
            self.items.sort_by_key(|&index| self.all[index].category());
        } else {
            // When: `query` is non-empty, every candidate is scored and ranked instead of listed.
            let mut matcher = Matcher::new(Config::DEFAULT);
            let pattern = Pattern::parse(&self.query, CaseMatching::Ignore, Normalization::Smart);
            let mut scratch: Vec<char> = Vec::new();
            let mut scored: Vec<(usize, u32)> = self
                .all
                .iter()
                .enumerate()
                .filter(|(_, entry)| !self.tabs_only || matches!(entry, PaletteEntry::Tab { .. }))
                .filter_map(|(i, _)| {
                    scratch.clear();
                    let haystack = Utf32Str::new(&self.presentation[i].search, &mut scratch);
                    pattern.score(haystack, &mut matcher).map(|s| (i, s))
                })
                .collect();
            scored.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            self.items = scored.into_iter().map(|(i, _)| i).collect();
        }
        if self.selected != NO_SELECTION && self.selected >= self.items.len() {
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
        Action::IncreaseFontWeight => "IncreaseFontWeight".into(),
        Action::DecreaseFontWeight => "DecreaseFontWeight".into(),
        Action::ResetFontWeight => "ResetFontWeight".into(),
        Action::SaveCurrentSettings => "SaveCurrentSettings".into(),
        Action::NewWindow => "NewWindow".into(),
        Action::MoveTabToNewWindow => "MoveTabToNewWindow".into(),
        Action::ToggleFullscreen => "ToggleFullscreen".into(),
        Action::QuitApp => "QuitApp".into(),
        Action::OpenSearch => "OpenSearch".into(),
        Action::OpenCommandPalette => "OpenCommandPalette".into(),
        Action::EditConfigFile => "EditConfigFile".into(),
        Action::OpenKeymapFile => "OpenKeymapFile".into(),
        Action::CheckForUpdates => "CheckForUpdates".into(),
        Action::Scroll(s) => format!("Scroll({})", scroll_name(*s)),
        Action::ScrollToPrevPrompt => "ScrollToPrevPrompt".into(),
        Action::ScrollToNextPrompt => "ScrollToNextPrompt".into(),
        Action::ReloadConfig => "ReloadConfig".into(),
        Action::OpenSshPane(t) => format!("OpenSshPane({t})"),
        Action::ApplyTheme(name) => format!("ApplyTheme({name})"),
        Action::ToggleTabBar => "ToggleTabBar".into(),
        Action::RenameTab => "RenameTab".into(),
        Action::UpdateTabColor => "UpdateTabColor".into(),
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
        Action::IncreaseFontWeight,
        Action::DecreaseFontWeight,
        Action::ResetFontWeight,
        Action::SaveCurrentSettings,
        // UI chrome
        Action::ToggleTabBar,
        Action::RenameTab,
        Action::UpdateTabColor,
        // Window
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
    ]
}

fn palette_accepts_keymap_action(action: &Action) -> bool {
    !matches!(action, Action::OpenSshPane(_))
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
#[path = "command_palette_tests.rs"]
mod command_palette_tests;
