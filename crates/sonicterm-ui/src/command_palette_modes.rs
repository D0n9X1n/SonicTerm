//! Rename, confirmation, color, status and font-family palette modes.

use super::*;

impl CommandPalette {
    /// Switch to tab-rename mode, seeding the field with the current title.
    pub fn start_rename_tab(&mut self, title_body: impl Into<String>) {
        self.session = self.session.wrapping_add(1);
        self.open = true;
        self.mode = CommandPaletteMode::RenameTab;
        self.query = title_body.into();
        self.selection = TextSelection::collapsed(self.query.len());
        self.items.clear();
        self.selected = 0;
        self.scroll_offset = 0;
    }

    /// Open the window-name editor with only the existing custom-name portion.
    pub fn start_rename_window(&mut self, name: impl Into<String>) {
        self.start_rename_tab(name);
        self.mode = CommandPaletteMode::RenameWindow;
        self.window_name_error = None;
    }

    /// Replace the selection with a whole text event atomically.
    ///
    /// The complete replacement candidate is validated before commit; a
    /// rejection preserves the query, caret, and anchor and records the error.
    pub fn input_window_name(&mut self, text: &str) {
        if text.is_empty() {
            // When: text is empty, a swallowed command chord must not clear rejected-input feedback.
            return;
        }
        let mut candidate = self.query.clone();
        let mut selection = self.selection;
        selection.replace(&mut candidate, text);
        match validate_window_name(&candidate) {
            Ok(_) => {
                self.query = candidate;
                self.selection = selection;
                self.window_name_error = None;
            }
            Err(error) => self.window_name_error = Some(error),
        }
    }

    /// Current rejection feedback, retained until a successful text edit or cancellation.
    pub fn window_name_error(&self) -> Option<WindowNameError> {
        self.window_name_error
    }

    /// Validate the editor's complete value without dismissing its existing rejection feedback.
    pub fn window_name_submission(&mut self) -> Option<String> {
        if self.window_name_error.is_some() {
            // When: window_name_error records rejected input, require correction before saving the remaining query.
            return None;
        }
        match validate_window_name(&self.query) {
            Ok(name) => Some(name.to_string()),
            Err(error) => {
                self.window_name_error = Some(error);
                None
            }
        }
    }

    /// Switch to tab-colour mode, listing `choices` for the named tab.
    pub fn start_tab_color_picker(
        &mut self,
        tab_title: impl Into<String>,
        choices: Vec<TabColorChoice>,
    ) {
        self.session = self.session.wrapping_add(1);
        self.open = true;
        self.mode = CommandPaletteMode::TabColor;
        self.query.clear();
        self.selection = TextSelection::default();
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

    /// Open a confirmation page: `apply` and `cancel` rows, then each affected item.
    ///
    /// Cancel is highlighted first, so Enter right after opening changes nothing.
    pub fn start_confirm(
        &mut self,
        title: impl Into<String>,
        apply: impl Into<String>,
        cancel: impl Into<String>,
        items: Vec<String>,
    ) {
        self.session = self.session.wrapping_add(1);
        self.open = true;
        self.mode = CommandPaletteMode::Confirm;
        self.tabs_only = false;
        self.window_name_error = None;
        self.query.clear();
        self.selection = TextSelection::default();
        self.confirm_title = title.into();
        self.confirm_rows =
            vec![ConfirmRow::Apply(apply.into()), ConfirmRow::Cancel(cancel.into())];
        self.confirm_rows.extend(items.into_iter().map(ConfirmRow::Item));
        self.items = (0..self.confirm_rows.len()).collect();
        self.selected = CONFIRM_CANCEL_ROW;
        self.scroll_offset = 0;
    }

    /// Title of the confirmation page.
    pub fn confirm_title(&self) -> &str {
        &self.confirm_title
    }

    /// Rows of the confirmation page in display order; empty in any other mode.
    pub fn confirm_rows(&self) -> &[ConfirmRow] {
        if self.mode != CommandPaletteMode::Confirm {
            // When: `mode` is not Confirm, stale rows from a closed page must not be shown.
            return &[];
        }
        &self.confirm_rows
    }

    /// Open the status line checklist with `rows`, highlighting `highlighted`'s row or else the
    /// first selectable one. `title` is the query placeholder and `footer` the key help.
    pub fn start_status_segments(
        &mut self,
        title: impl Into<String>,
        footer: impl Into<String>,
        rows: Vec<StatusSegmentRow>,
        highlighted: Option<sonicterm_cfg::status_line::StatusSegmentId>,
    ) {
        self.session = self.session.wrapping_add(1);
        self.open = true;
        self.mode = CommandPaletteMode::StatusSegments;
        self.tabs_only = false;
        self.window_name_error = None;
        self.query.clear();
        self.selection = TextSelection::default();
        self.status_title = title.into();
        self.status_footer = footer.into();
        self.status_rows = rows;
        self.scroll_offset = 0;
        self.filter_status_rows();
        let wanted = highlighted.map(StatusSegmentRowKind::Segment);
        self.selected = wanted
            .and_then(|kind| {
                self.items.iter().position(|&index| self.status_rows[index].kind == kind)
            })
            .unwrap_or_else(|| self.first_selectable_status_row());
        self.ensure_selected_in_view();
    }

    /// Replace the checklist rows after a change, keeping the query and the highlighted row.
    pub fn update_status_segments(&mut self, rows: Vec<StatusSegmentRow>) {
        if self.mode != CommandPaletteMode::StatusSegments {
            // When: mode is not StatusSegments, the checklist closed; there is nothing to refresh.
            return;
        }
        let kept = self.selected_status_row();
        let scroll = self.scroll_offset;
        self.status_rows = rows;
        self.filter_status_rows();
        self.selected = kept
            .and_then(|kind| {
                self.items.iter().position(|&index| self.status_rows[index].kind == kind)
            })
            .unwrap_or_else(|| self.first_selectable_status_row());
        self.scroll_offset = scroll;
        self.ensure_selected_in_view();
    }

    /// The checklist rows the query keeps, in display order; empty in any other mode.
    pub fn status_segment_rows(&self) -> Vec<&StatusSegmentRow> {
        if self.mode != CommandPaletteMode::StatusSegments {
            // When: mode is not StatusSegments, stale rows from a closed checklist must not show.
            return Vec::new();
        }
        self.items.iter().filter_map(|&index| self.status_rows.get(index)).collect()
    }

    /// What the highlighted checklist row does, if the checklist is open and a row is highlighted.
    pub fn selected_status_row(&self) -> Option<StatusSegmentRowKind> {
        if self.mode != CommandPaletteMode::StatusSegments {
            // When: mode is not StatusSegments, no checklist row is highlighted.
            return None;
        }
        let index = *self.items.get(self.selected)?;
        self.status_rows.get(index).filter(|row| row.selectable()).map(|row| row.kind)
    }

    /// Highlight checklist row `index` of the filtered view; a heading is never highlighted.
    pub fn select_status_row(&mut self, index: usize) -> bool {
        let selectable = self.mode == CommandPaletteMode::StatusSegments
            && self
                .items
                .get(index)
                .and_then(|&row| self.status_rows.get(row))
                .is_some_and(StatusSegmentRow::selectable);
        if selectable {
            self.selected = index;
            self.ensure_selected_in_view();
        }
        selectable
    }

    /// The checklist's query placeholder.
    pub fn status_title(&self) -> &str {
        &self.status_title
    }

    /// The checklist's key help.
    pub fn status_footer(&self) -> &str {
        &self.status_footer
    }

    /// Open the font family picker with `rows`, highlighting `current`'s row or else the first.
    /// `title` is the query placeholder and `footer` the key help.
    pub fn start_font_family(
        &mut self,
        title: impl Into<String>,
        footer: impl Into<String>,
        rows: Vec<FontFamilyRow>,
        current: &str,
    ) {
        self.session = self.session.wrapping_add(1);
        self.open = true;
        self.mode = CommandPaletteMode::FontFamily;
        self.tabs_only = false;
        self.window_name_error = None;
        self.query.clear();
        self.selection = TextSelection::default();
        self.font_title = title.into();
        self.font_footer = footer.into();
        self.font_rows = rows;
        self.scroll_offset = 0;
        self.filter_font_rows();
        self.selected = self
            .items
            .iter()
            .position(|&index| self.font_rows[index].family.eq_ignore_ascii_case(current))
            .unwrap_or(0);
        self.ensure_selected_in_view();
    }

    /// The picker rows the query keeps, in display order; empty in any other mode.
    pub fn font_family_rows(&self) -> Vec<&FontFamilyRow> {
        if self.mode != CommandPaletteMode::FontFamily {
            // When: mode is not FontFamily, stale rows from a closed picker must not show.
            return Vec::new();
        }
        self.items.iter().filter_map(|&index| self.font_rows.get(index)).collect()
    }

    /// The highlighted picker family, if the picker is open and a row is highlighted.
    pub fn selected_font_family(&self) -> Option<&str> {
        if self.mode != CommandPaletteMode::FontFamily {
            // When: mode is not FontFamily, no picker row is highlighted.
            return None;
        }
        let index = *self.items.get(self.selected)?;
        self.font_rows.get(index).map(|row| row.family.as_str())
    }

    /// Replace the picker's title and rows in place, keeping the typed filter and, when it is
    /// still listed, the highlighted family; does nothing once the picker has closed.
    pub fn update_font_family(&mut self, title: impl Into<String>, rows: Vec<FontFamilyRow>) {
        if self.mode != CommandPaletteMode::FontFamily {
            // When: mode is not FontFamily, the picker closed; there is nothing to refresh.
            return;
        }
        let kept = self.selected_font_family().map(str::to_owned);
        self.font_title = title.into();
        self.font_rows = rows;
        self.filter_font_rows();
        self.selected = kept
            .and_then(|family| {
                self.items.iter().position(|&index| self.font_rows[index].family == family)
            })
            .unwrap_or(0);
        self.ensure_selected_in_view();
    }

    /// Highlight picker row `index` of the filtered view.
    pub fn select_font_row(&mut self, index: usize) -> bool {
        let selectable = self.mode == CommandPaletteMode::FontFamily && index < self.items.len();
        if selectable {
            self.selected = index;
            self.ensure_selected_in_view();
        }
        selectable
    }

    /// The font picker's query placeholder.
    pub fn font_title(&self) -> &str {
        &self.font_title
    }

    /// The font picker's key help.
    pub fn font_footer(&self) -> &str {
        &self.font_footer
    }

    /// Keep every picker row for an empty query, else the families containing it, without case.
    pub(super) fn filter_font_rows(&mut self) {
        let query = self.query.trim().to_lowercase();
        self.items = (0..self.font_rows.len())
            .filter(|&index| {
                query.is_empty() || self.font_rows[index].family.to_lowercase().contains(&query)
            })
            .collect();
    }

    /// Keep every checklist row for an empty query, else the segment rows it matches.
    pub(super) fn filter_status_rows(&mut self) {
        let query = self.query.trim().to_lowercase();
        self.items = (0..self.status_rows.len())
            .filter(|&index| query.is_empty() || self.status_rows[index].matches(&query))
            .collect();
    }

    /// The filtered index of the first selectable checklist row, or 0 when none is left.
    pub(super) fn first_selectable_status_row(&self) -> usize {
        self.items.iter().position(|&index| self.status_rows[index].selectable()).unwrap_or(0)
    }

    /// Step the checklist highlight one selectable row forward or back, wrapping at both ends.
    pub(super) fn step_status_selection(&mut self, forward: bool) {
        let count = self.items.len();
        for step in 1..=count {
            let candidate = if forward {
                (self.selected + step) % count
            } else {
                // When: forward is false, step back without underflowing past the first row.
                (self.selected + count - step % count) % count
            };
            if self.status_rows[self.items[candidate]].selectable() {
                // When: the candidate row is selectable, it becomes the highlight and the search stops.
                self.selected = candidate;
                break;
            }
        }
        self.ensure_selected_in_view();
    }

    /// Refilter after the query changed: commands, or the checklist's segment rows.
    pub(super) fn refilter_typed(&mut self) {
        match self.mode {
            CommandPaletteMode::Commands => self.refilter(),
            CommandPaletteMode::StatusSegments => {
                self.filter_status_rows();
                self.selected = self.first_selectable_status_row();
            }
            CommandPaletteMode::FontFamily => {
                self.filter_font_rows();
                self.selected = 0;
            }
            _ => {
                // When: mode edits a name or shows a fixed list, typing filters nothing.
            }
        }
    }

    /// Whether the highlighted row of an open confirmation page is Apply.
    pub fn confirm_applies(&self) -> bool {
        self.mode == CommandPaletteMode::Confirm
            && matches!(self.confirm_rows.get(self.selected), Some(ConfirmRow::Apply(_)))
    }
}

#[cfg(test)]
#[path = "command_palette_modes_tests.rs"]
mod command_palette_modes_tests;
