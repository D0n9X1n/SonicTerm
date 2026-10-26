use super::*;

impl Grid {
    /// Put a character at cursor, advancing cursor by character width.
    pub fn put_char(&mut self, character: char, fg: Color, bg: Color, flags: CellFlags) {
        self.put_char_linked(character, fg, bg, flags, None);
    }

    /// Put a character at cursor, also tagging the cell(s) with an optional
    /// hyperlink id.
    pub fn put_char_linked(
        &mut self,
        character: char,
        fg: Color,
        bg: Color,
        flags: CellFlags,
        hyperlink: Option<HyperlinkId>,
    ) {
        self.put_char_styled(character, fg, bg, flags, hyperlink, UnderlineStyle::Single, None);
    }

    /// Put a character at cursor with hyperlink and underline metadata.
    ///
    /// The extra underline fields are stored in [`Cell`]'s rare-attribute box
    /// only when they differ from terminal defaults, preserving the normal hot
    /// path for plain cells.
    #[allow(clippy::too_many_arguments)]
    pub fn put_char_styled(
        &mut self,
        character: char,
        fg: Color,
        bg: Color,
        flags: CellFlags,
        hyperlink: Option<HyperlinkId>,
        underline_style: UnderlineStyle,
        underline_color: Option<Color>,
    ) {
        self.put_char_styled_in_region(
            character,
            fg,
            bg,
            flags,
            hyperlink,
            underline_style,
            underline_color,
            None,
            Cell::default(),
        );
    }

    /// Put a styled character at the cursor, wrapping and filling within an optional scroll region.
    #[allow(clippy::too_many_arguments)]
    pub fn put_char_styled_in_region(
        &mut self,
        character: char,
        fg: Color,
        bg: Color,
        flags: CellFlags,
        hyperlink: Option<HyperlinkId>,
        underline_style: UnderlineStyle,
        underline_color: Option<Color>,
        wrap_region: Option<(u16, u16)>,
        wrap_fill: Cell,
    ) {
        // ASCII printable fast-path: every codepoint in 0x20..=0x7E has
        // unicode_width == 1 unconditionally. `UnicodeWidthChar::width`
        // performs a binary search through several MiB of tables — for
        // shell output (overwhelmingly ASCII) this dominates the
        // parser hot path. Skipping it here recovers ~30% of
        // parse_ns_per_byte on bursty scroll workloads.
        let width = if (character as u32) >= 0x20 && (character as u32) <= 0x7E {
            1u16
        } else {
            // When: `character` is outside printable ASCII, look up its Unicode width.
            unicode_width::UnicodeWidthChar::width(character).unwrap_or(1) as u16
        };
        // Zero-width codepoints (ZWJ U+200D, combining marks, etc.) belong to
        // the current grapheme and must not advance the cursor.
        if width == 0 {
            // When: `width == 0`, extend the preceding grapheme without moving the cursor.

            // It must NOT advance the cursor, but it IS part of the
            // current cluster — the shaper needs to see it together
            // with the lead char so ZWJ sequences like 👨‍👩‍👧 actually
            // compose. Attach it to the previous lead cell on this row.
            // If the cursor is at column 0 (no prior cell on this row)
            // there is no cluster to attach to and the codepoint is
            // dropped — matches every other terminal's behavior.
            if self.cursor.col == 0 {
                // When: `self.cursor.col == 0`, no preceding grapheme exists on this row.
                return;
            }
            let row = self.cursor.row as usize;
            // Walk back past any WIDE_CONT cells to find the lead.
            let mut column = self.cursor.col as usize - 1;
            while column > 0 && self.visible[row][column].flags.contains(CellFlags::WIDE_CONT) {
                column -= 1;
            }
            // Reaching column zero on a continuation means there is no lead
            // cell to attach this codepoint to safely.
            if self.visible[row][column].flags.contains(CellFlags::WIDE_CONT) {
                // When: `self.visible[row][column]` is still `WIDE_CONT`, no valid lead cell exists.
                return;
            }
            let lead = &mut self.visible[row][column];
            let existing_len = lead.extras().map_or(0, str::len);
            if existing_len.saturating_add(character.len_utf8()) > MAX_CELL_EXTRAS_BYTES {
                // When: `existing_len + character.len_utf8()` exceeds `MAX_CELL_EXTRAS_BYTES`, drop `character`.
                return;
            }
            let mut extras = match lead.take_extras() {
                Some(boxed) => String::from(boxed),
                None => String::new(),
            };
            extras.push(character);
            lead.set_extras(Some(extras.into_boxed_str()));
            // A wide lead cell also covers the column after `column`.
            let lead_width =
                if self.visible[row][column].flags.contains(CellFlags::WIDE) { 2 } else { 1 };
            self.mark_row(self.cursor.row);
            self.content_changed_row(self.cursor.row);
            // A changed glyph in the last column revokes the wrap this row passes to the next.
            if column + lead_width >= usize::from(self.cols) {
                self.revoke_successor_wrap(self.cursor.row);
            }
            self.bump();
            return;
        }
        if self.pending_wrap {
            self.pending_wrap = false;
            if self.autowrap {
                self.wrap_linefeed_with(wrap_region, wrap_fill.clone());
                self.cursor.col = 0;
            }
        }
        let mut effective_width = width;
        if self.cursor.col + width > self.cols {
            if self.autowrap {
                self.wrap_linefeed_with(wrap_region, wrap_fill.clone());
                self.cursor.col = 0;
                if width > self.cols {
                    effective_width = 1;
                }
            } else {
                // When: `self.autowrap` is false on overflow, overwrite the final column.
                self.cursor.col = self.cols.saturating_sub(1);
                effective_width = 1;
            }
        }
        let (row, column) = (self.cursor.row as usize, self.cursor.col as usize);
        let mut clean_flags = flags;
        clean_flags.remove(CellFlags::WIDE | CellFlags::WIDE_CONT);
        let mut fill = Cell::plain(' ', fg, bg, clean_flags);
        Self::apply_rare_attrs(&mut fill, hyperlink, underline_style, underline_color);
        // Writing at column 0 keeps this row's incoming wrap; only its last column carries the outgoing one.
        let (_, cleared_end) =
            self.clear_wide_intersections(row, column, effective_width as usize, fill);

        let cell_flags = if effective_width == 2 {
            clean_flags | CellFlags::WIDE
        } else {
            // When: `effective_width != 2`, retain the ordinary cell flags.
            clean_flags
        };
        let mut lead = Cell::plain(character, fg, bg, cell_flags);
        Self::apply_rare_attrs(&mut lead, hyperlink, underline_style, underline_color);
        self.visible[row][column] = lead;
        if effective_width == 2 && column + 1 < self.cols as usize {
            let mut cont = Cell::plain(' ', fg, bg, clean_flags | CellFlags::WIDE_CONT);
            Self::apply_rare_attrs(&mut cont, hyperlink, underline_style, underline_color);
            self.visible[row][column + 1] = cont;
        }
        self.cursor.col += effective_width;
        if self.cursor.col >= self.cols {
            if self.autowrap {
                self.cursor.col = self.cols;
                self.pending_wrap = true;
            } else {
                // When: `self.autowrap` is false at the right edge, pin the cursor to the last column.
                self.cursor.col = self.cols.saturating_sub(1);
            }
        }
        self.mark_row(row as u16);
        self.content_changed_row(row as u16);
        // A write or wide-cell repair that reaches the last column revokes the outgoing wrap.
        if cleared_end.max(column + usize::from(effective_width)) >= usize::from(self.cols) {
            self.revoke_successor_wrap(row as u16);
        }
        self.bump();
    }

    pub(super) fn wide_expanded_range(
        &self,
        row: usize,
        start: usize,
        width: usize,
    ) -> (usize, usize) {
        let cols = self.cols as usize;
        let mut low = start.min(cols);
        let mut high = (start + width).min(cols);
        if low >= high {
            // When: `low >= high`, the requested span cannot intersect a wide pair.
            return (low, high);
        }
        loop {
            let old = (low, high);
            let scan = low..high;
            for column in scan {
                let cell = &self.visible[row][column];
                if cell.flags.contains(CellFlags::WIDE) {
                    high = high.max((column + 2).min(cols));
                } else if cell.flags.contains(CellFlags::WIDE_CONT) && column > 0 {
                    // When: `cell.flags` contains `WIDE_CONT` and `column > 0`, include its lead cell.
                    low = low.min(column - 1);
                }
            }
            if (low, high) == old {
                // When: `(low, high) == old`, the scan found the complete wide-cell span.
                return (low, high);
            }
        }
    }

    fn clear_wide_intersections(
        &mut self,
        row: usize,
        start: usize,
        width: usize,
        fill: Cell,
    ) -> (usize, usize) {
        let range = self.wide_expanded_range(row, start, width);
        for column in range.0..range.1 {
            self.visible[row][column] = fill.clone();
        }
        range
    }

    pub(super) fn repair_wide_row(&mut self, row: usize, fill: Cell) {
        let cols = self.cols as usize;
        for column in 0..cols {
            let flags = self.visible[row][column].flags;
            if flags.contains(CellFlags::WIDE) {
                if column + 1 >= cols
                    || !self.visible[row][column + 1].flags.contains(CellFlags::WIDE_CONT)
                {
                    self.visible[row][column] = fill.clone();
                }
            } else if flags.contains(CellFlags::WIDE_CONT)
                && (column == 0 || !self.visible[row][column - 1].flags.contains(CellFlags::WIDE))
            {
                // When: `flags` has `WIDE_CONT` without a preceding `WIDE`, replace the orphan.
                self.visible[row][column] = fill.clone();
            }
        }
    }

    #[inline]
    fn apply_rare_attrs(
        cell: &mut Cell,
        hyperlink: Option<HyperlinkId>,
        underline_style: UnderlineStyle,
        underline_color: Option<Color>,
    ) {
        cell.set_hyperlink(hyperlink);
        cell.set_underline_style(underline_style);
        cell.set_underline_color(underline_color);
    }

    fn wrap_linefeed_with(&mut self, region: Option<(u16, u16)>, fill: Cell) {
        if let Some((top, bottom)) = region {
            // When: `region` is `Some((top, bottom))`, apply its bottom margin.
            if self.cursor.row == bottom {
                // When: `self.cursor.row == bottom`, scroll the region before marking the new continuation row.
                self.scroll_region_up_with(top, bottom, 1, fill);
                self.set_soft_wrapped_from_previous(self.cursor.row, true);
                return;
            }
        }
        self.linefeed_with(fill);
        self.set_soft_wrapped_from_previous(self.cursor.row, true);
    }

    /// Move the cursor to column 0 of the current row.
    pub fn carriage_return(&mut self) {
        self.clear_pending_wrap();
        self.cursor.col = 0;
        let row = self.cursor.row;
        self.mark_row(row);
        self.bump();
    }

    /// Advance the cursor one row, scrolling the visible region if needed.
    pub fn linefeed(&mut self) {
        self.linefeed_with(Cell::default());
    }

    /// Advance the cursor one row, using `fill` for any row newly exposed by
    /// scrolling. This models terminals with background-color erase (BCE):
    /// when a colored app scrolls, the blank line inherits the active SGR
    /// rendition instead of reverting to the theme default.
    pub fn linefeed_with(&mut self, fill: Cell) {
        self.clear_pending_wrap();
        let old = self.cursor.row;
        // `scroll_up_with` already marks every row dirty when linefeed starts
        // at the bottom edge.
        if self.cursor.row + 1 >= self.rows {
            self.scroll_up_with(1, fill);
        } else {
            // When: `self.cursor.row + 1 < self.rows`, advance without scrolling.
            self.cursor.row += 1;
        }
        // Both leaving and arriving rows count as touched (cursor moved
        // through them, even if the cells themselves are unchanged — the
        // renderer may need to redraw the cursor on either).
        self.mark_row(old);
        self.mark_row(self.cursor.row);
        self.set_soft_wrapped_from_previous(self.cursor.row, false);
        self.bump();
    }

    /// Move the cursor one column to the left (does not erase).
    pub fn backspace(&mut self) {
        self.clear_pending_wrap();
        self.cursor.col = self.cursor.col.saturating_sub(1);
        let row = self.cursor.row;
        self.mark_row(row);
        self.bump();
    }

    /// Advance the cursor to the next tab stop (8-column tabs).
    pub fn tab(&mut self) {
        self.clear_pending_wrap();
        let next = ((self.cursor.col / 8) + 1) * 8;
        self.cursor.col = next.min(self.cols.saturating_sub(1));
        let row = self.cursor.row;
        self.mark_row(row);
        self.bump();
    }

    /// Move cursor to (row, col), clamping to grid bounds.
    pub fn goto(&mut self, row: u16, col: u16) {
        self.clear_pending_wrap();
        let old_row = self.cursor.row;
        self.cursor.row = row.min(self.rows.saturating_sub(1));
        self.cursor.col = col.min(self.cols.saturating_sub(1));
        // Both leaving and arriving rows need re-render so the cursor
        // quad doesn't lag behind.
        self.mark_row(old_row);
        self.mark_row(self.cursor.row);
        self.bump();
    }

    /// Move through an explicit hard-line control without scrolling the grid.
    pub fn goto_hard_line(&mut self, row: u16, col: u16) {
        self.goto(row, col);
        self.set_soft_wrapped_from_previous(self.cursor.row, false);
    }
}

#[cfg(test)]
#[path = "grid_write_tests.rs"]
mod grid_write_tests;
