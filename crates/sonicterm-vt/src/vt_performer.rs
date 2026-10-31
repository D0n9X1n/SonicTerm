use super::*;

fn parse_ext_color(slice: &[u16], iter: &mut vte::ParamsIter<'_>) -> Option<Color> {
    if slice.len() > 1 {
        // When: slice contains colon subparameters, parse it alone so malformed colors cannot consume the next SGR.
        return match slice {
            [_, 5, index] => Some(Color::Indexed(u8::try_from(*index).ok()?)),
            [_, 2, red, green, blue] | [_, 2, 0, red, green, blue] => Some(Color::Rgb(
                u8::try_from(*red).ok()?,
                u8::try_from(*green).ok()?,
                u8::try_from(*blue).ok()?,
            )),
            _ => None,
        };
    }
    // Semicolon parameters retain their existing consumption and conversion behavior.
    let mode = iter.next()?.first().copied()?;
    match mode {
        5 => Some(Color::Indexed(iter.next()?.first().copied()? as u8)),
        2 => {
            let red = iter.next()?.first().copied()? as u8;
            let green = iter.next()?.first().copied()? as u8;
            let blue = iter.next()?.first().copied()? as u8;
            Some(Color::Rgb(red, green, blue))
        }
        _ => None,
    }
}

impl Performer {
    pub(super) fn new(
        grid: Grid,
        reply_tx: Option<Sender<Vec<u8>>>,
        staging_pool: Arc<CaptureStagingPool>,
    ) -> Self {
        Self {
            grid,
            fg: Color::Default,
            bg: Color::Default,
            flags: CellFlags::empty(),
            underline_style: UnderlineStyle::Single,
            underline_color: None,
            events: Vec::new(),
            hyperlinks: HyperlinkRegistry::new(),
            current_hyperlink: None,
            hyperlink_reclaim_scanned_at: 0,
            hyperlink_reclaim_backoff: 0,
            saved_cursor: None,
            bracketed_paste: false,
            mouse_sgr: false,
            app_cursor_keys: false,
            app_keypad: false,
            backarrow_key: false,
            newline_mode: false,
            modify_other_keys: 0,
            win32_input: false,
            keyboard_protocol_epoch: 0,
            mouse_tracking: MouseTracking::default(),
            focus_reporting: false,
            synchronized_output: false,
            sync_epoch: 0,
            sync_resets: 0,
            title: None,
            cwd: None,
            osc7_cwd: None,
            cwd_revision: 0,
            reply_tx,
            captured_replies: std::cell::RefCell::new(None),
            reply_queue_full_warned: std::sync::atomic::AtomicBool::new(false),
            theme_fg: None,
            theme_bg: None,
            theme_cursor: None,
            theme_palette: [None; 16],
            suppress_next_osc4: false,
            scroll_top: None,
            scroll_bottom: None,
            fast_path_ready: true,
            last_printed_char: None,
            dcs_capture: None,
            staging_pool,
            kitty_transfer: None,
            decrqss: None,
            decrqss_awaiting_backslash: false,
            kitty_kbd_flags: [Vec::new(), Vec::new()],
        }
    }

    /// Resolve the active scroll region, defaulting to the full
    /// visible grid when DECSTBM has not been set. Used by every
    /// scroll-emitting opcode (CSI S, CSI T, IND-at-bottom-margin,
    /// RI-at-top-margin).
    pub(super) fn effective_scroll_region(&self) -> (u16, u16) {
        let rows = self.grid.rows;
        let top = self.scroll_top.unwrap_or(0);
        let bot = self.scroll_bottom.unwrap_or(rows.saturating_sub(1));
        (top, bot)
    }

    /// Queue a media event stamped with the grid's scroll count, so the host can keep an image
    /// anchored at the event's row beside the text it was drawn with as the screen scrolls.
    pub(super) fn push_media(&mut self, mut event: MediaEvent) {
        event.scroll_mark = self.grid.lines_scrolled();
        self.events.push(VtEvent::Media(event));
    }

    pub(super) fn physical_cursor_col(&self) -> u16 {
        self.grid.cursor.col.min(self.grid.cols.saturating_sub(1))
    }

    pub(super) fn advance_hard_line(&mut self, fill: Cell, carriage_return: bool) {
        let (top, bottom) = self.effective_scroll_region();
        let col = self.physical_cursor_col();
        let col = if carriage_return { 0 } else { col };
        let mut row = self.grid.cursor.row;
        if row == bottom {
            // The control's fill applies only to the newly exposed row inside the active region.
            self.grid.scroll_region_up_with(top, bottom, 1, fill);
        } else {
            // When: `row != bottom`, physical bounds protect rows below the scroll region.
            row = row.saturating_add(1).min(self.grid.rows.saturating_sub(1));
        }
        self.grid.goto_hard_line(row, col);
    }

    // Ordering: reply_queue_full_warned uses Relaxed because it suppresses duplicate warnings without publishing reply bytes.
    pub(super) fn reply(&self, bytes: &[u8]) {
        if let Some(captured) = self.captured_replies.borrow_mut().as_mut() {
            // When: captured_replies is enabled, stage one dispatch before yielding; OSC 4 input is capped at 4096 bytes.
            captured.extend_from_slice(bytes);
            return;
        }
        if let Some(tx) = &self.reply_tx {
            match tx.try_send(bytes.to_vec()) {
                Ok(()) => {
                    self.reply_queue_full_warned.store(false, std::sync::atomic::Ordering::Relaxed);
                }
                Err(crossbeam_channel::TrySendError::Full(_)) => {
                    if !self
                        .reply_queue_full_warned
                        .swap(true, std::sync::atomic::Ordering::Relaxed)
                    {
                        tracing::warn!("terminal reply dropped because the reply queue is full");
                    }
                }
                Err(crossbeam_channel::TrySendError::Disconnected(_)) => {
                    // When: TrySendError::Disconnected means no host can receive bytes, so dropping the reply avoids blocking parser progress.
                }
            }
        }
    }

    pub(super) fn handle_osc4_raw(&self, raw_pairs: &[u8], bell_terminated: bool) {
        let terminator: &[u8] = if bell_terminated { b"\x07" } else { b"\x1b\\" };
        let mut parts = raw_pairs.split(|&byte| byte == b';');
        while let Some(idx) = parts.next() {
            let Some(spec) = parts.next() else {
                // When: spec is missing from the index/spec pair, stop before replying with a mismatched palette entry.
                break;
            };
            let idx =
                std::str::from_utf8(idx).ok().and_then(|digits| digits.trim().parse::<u8>().ok());
            let spec = std::str::from_utf8(spec).ok().map(str::trim);
            if let (Some(idx), Some("?")) = (idx, spec) {
                self.reply_osc4_query(idx, terminator);
            }
        }
    }

    pub(super) fn handle_osc4_pairs(&self, params: &[&[u8]], bell_terminated: bool) {
        let terminator: &[u8] = if bell_terminated { b"\x07" } else { b"\x1b\\" };
        let mut param_index = 1;
        while param_index + 1 < params.len() {
            let idx = std::str::from_utf8(params[param_index])
                .ok()
                .and_then(|digits| digits.trim().parse::<u8>().ok());
            let spec = std::str::from_utf8(params[param_index + 1]).ok().map(str::trim);
            if let (Some(idx), Some("?")) = (idx, spec) {
                self.reply_osc4_query(idx, terminator);
            }
            param_index += 2;
        }
    }

    fn reply_osc4_query(&self, idx: u8, terminator: &[u8]) {
        if let Some(Some((red, green, blue))) = self.theme_palette.get(idx as usize).copied() {
            let mut buf = Vec::with_capacity(28);
            buf.extend_from_slice(b"\x1b]4;");
            buf.extend_from_slice(idx.to_string().as_bytes());
            buf.extend_from_slice(
                format!(";rgb:{red:02x}{red:02x}/{green:02x}{green:02x}/{blue:02x}{blue:02x}")
                    .as_bytes(),
            );
            buf.extend_from_slice(terminator);
            self.reply(&buf);
        }
    }

    pub(super) fn reset_last_printed_char(&mut self) {
        self.last_printed_char = None;
    }

    /// Blank cell with current colors but no underline. This is the Sonic Grid
    /// equivalent of WezTerm/xterm background-color erase (BCE): ED/EL/ECH,
    /// inserted blanks, deleted-cell fill, and scroll-fill rows inherit the
    /// app's active colors instead of falling back to the terminal theme.
    pub(super) fn erase_fill_cell(&self) -> Cell {
        let mut flags = self.flags;
        // Erased blanks retain colors, not underline ink; explicitly printed spaces keep their SGR rendition.
        flags.remove(CellFlags::WIDE | CellFlags::WIDE_CONT | CellFlags::UNDERLINE);
        let mut cell = Cell::plain(' ', self.fg, self.bg, flags);
        cell.set_hyperlink(self.current_hyperlink);
        cell
    }

    pub(super) fn print_graphic(&mut self, character: char) {
        let region = (self.scroll_top.is_some() || self.scroll_bottom.is_some())
            .then(|| self.effective_scroll_region());
        let fill = self.erase_fill_cell();
        self.grid.put_char_styled_in_region(
            character,
            self.fg,
            self.bg,
            self.flags,
            self.current_hyperlink,
            self.underline_style,
            self.underline_color,
            region,
            fill,
        );
        self.last_printed_char = Some(character);
    }

    pub(super) fn clear_decrqss(&mut self) {
        self.decrqss = None;
        self.decrqss_awaiting_backslash = false;
    }

    pub(super) fn observe_decrqss_terminator(&mut self, byte: u8) {
        let Some(request) = self.decrqss else {
            // When: decrqss is absent, ordinary escape and media handling retain byte ownership.
            return;
        };
        let complete = if self.decrqss_awaiting_backslash {
            self.clear_decrqss();
            byte == b'\\'
        } else {
            // When: decrqss_awaiting_backslash is false, distinguish payload from cancellation or the start of ST.
            match byte {
                0x9c => {
                    self.clear_decrqss();
                    true
                }
                0x1b => {
                    self.decrqss_awaiting_backslash = true;
                    false
                }
                0x18 | 0x1a => {
                    self.clear_decrqss();
                    false
                }
                0x7f..=0xff => {
                    // Ignored vte payload bytes must still invalidate the selector.
                    self.decrqss = Some(DecrqssRequest::Unsupported);
                    false
                }
                _ => false,
            }
        };
        if complete {
            // When: complete is true, request selects a truthful Sgr reply or an unsupported-selector response.
            if request == DecrqssRequest::Sgr {
                // Serialize the current rendition rather than an assumed probe result.
                self.reply_current_sgr();
            } else {
                // Unsupported selectors must not invent a status string.
                self.reply(b"\x1bP0$r\x1b\\");
            }
        }
    }

    fn reply_current_sgr(&self) {
        use std::fmt::Write as _;

        // Fixed rendition fields bound this reply independently of request length.
        let mut reply = String::from("\x1bP1$r0");
        for (flag, code) in [(CellFlags::BOLD, 1), (CellFlags::DIM, 2), (CellFlags::ITALIC, 3)] {
            if self.flags.contains(flag) {
                // When: flags contains this rendition bit, include its SGR code after the reset prefix.
                let _ = write!(reply, ";{code}");
            }
        }
        if self.flags.contains(CellFlags::UNDERLINE) {
            // When: UNDERLINE is active, preserve its exact style in the colon-form SGR selector.
            let style = match self.underline_style {
                UnderlineStyle::Single => 1,
                UnderlineStyle::Double => 2,
                UnderlineStyle::Curly => 3,
                UnderlineStyle::Dotted => 4,
                UnderlineStyle::Dashed => 5,
            };
            let _ = write!(reply, ";4:{style}");
        }
        for (flag, code) in [
            (CellFlags::BLINK, 5),
            (CellFlags::INVERSE, 7),
            (CellFlags::HIDDEN, 8),
            (CellFlags::STRIKETHROUGH, 9),
        ] {
            if self.flags.contains(flag) {
                // When: flags contains this rendition bit, include its SGR code after the reset prefix.
                let _ = write!(reply, ";{code}");
            }
        }
        for (code, color) in
            [(38, self.fg), (48, self.bg), (58, self.underline_color.unwrap_or(Color::Default))]
        {
            // String formatting is infallible; default colors are already represented by SGR 0.
            match color {
                Color::Default => {
                    // When: color is Default, the leading SGR reset already selects it.
                }
                Color::Indexed(index) => {
                    // When: color is Indexed, report its palette index rather than resolving it to RGB.
                    let _ = write!(reply, ";{code};5;{index}");
                }
                Color::Rgb(red, green, blue) => {
                    // When: color is Rgb, preserve all three truecolor components.
                    let _ = write!(reply, ";{code};2;{red};{green};{blue}");
                }
            }
        }
        reply.push_str("m\x1b\\");
        self.reply(reply.as_bytes());
    }

    fn reset_attrs(&mut self) {
        self.fg = Color::Default;
        self.bg = Color::Default;
        self.flags = CellFlags::empty();
        self.underline_style = UnderlineStyle::Single;
        self.underline_color = None;
    }

    pub(super) fn reset_terminal(&mut self) {
        // A reset program leaves no progress behind; the host clears it.
        self.events.push(VtEvent::ProgramStateReset);
        // RIS cancels prior held input even when Win32 was already inactive.
        self.advance_keyboard_protocol_epoch();
        self.win32_input = false;
        self.reset_attrs();
        self.saved_cursor = None;
        self.bracketed_paste = false;
        self.mouse_sgr = false;
        self.app_cursor_keys = false;
        self.app_keypad = false;
        self.backarrow_key = false;
        self.newline_mode = false;
        self.modify_other_keys = 0;
        self.mouse_tracking = MouseTracking::Off;
        self.focus_reporting = false;
        self.set_synchronized_output(false);
        self.current_hyperlink = None;
        self.scroll_top = None;
        self.scroll_bottom = None;
        self.last_printed_char = None;
        self.dcs_capture = None;
        self.clear_decrqss();
        for stack in &mut self.kitty_kbd_flags {
            stack.clear();
        }
        self.grid.set_autowrap(true);
        if self.grid.is_alt() {
            self.grid.leave_alt_screen();
        }
        self.grid.erase_screen_with(Cell::default());
        self.grid.goto(0, 0);
        // RIS erases every screen and drops the alt buffer above, so no cell
        // can still reference an interned link. Clearing outright is cheaper
        // and no less correct than scanning to prove the set is empty.
        self.hyperlinks.clear();
        self.hyperlink_reclaim_backoff = 0;
    }

    pub(super) fn apply_sgr(&mut self, params: &Params) {
        let mut iter = params.iter();
        while let Some(slice) = iter.next() {
            let parameter = slice.first().copied().unwrap_or(0);
            // When: parameter selects a supported SGR mutation; unsupported values leave the current rendition unchanged for forward compatibility.
            match parameter {
                0 => self.reset_attrs(),
                1 => self.flags |= CellFlags::BOLD,
                2 => self.flags |= CellFlags::DIM,
                3 => self.flags |= CellFlags::ITALIC,
                4 => {
                    let style = slice.get(1).copied().unwrap_or(1);
                    match style {
                        0 => {
                            self.flags.remove(CellFlags::UNDERLINE);
                            self.underline_style = UnderlineStyle::Single;
                        }
                        1 => {
                            self.flags |= CellFlags::UNDERLINE;
                            self.underline_style = UnderlineStyle::Single;
                        }
                        2 => {
                            self.flags |= CellFlags::UNDERLINE;
                            self.underline_style = UnderlineStyle::Double;
                        }
                        3 => {
                            self.flags |= CellFlags::UNDERLINE;
                            self.underline_style = UnderlineStyle::Curly;
                        }
                        4 => {
                            self.flags |= CellFlags::UNDERLINE;
                            self.underline_style = UnderlineStyle::Dotted;
                        }
                        5 => {
                            self.flags |= CellFlags::UNDERLINE;
                            self.underline_style = UnderlineStyle::Dashed;
                        }
                        _ => {
                            self.flags |= CellFlags::UNDERLINE;
                            self.underline_style = UnderlineStyle::Single;
                        }
                    }
                }
                5 => self.flags |= CellFlags::BLINK,
                7 => self.flags |= CellFlags::INVERSE,
                8 => self.flags |= CellFlags::HIDDEN,
                9 => self.flags |= CellFlags::STRIKETHROUGH,
                21 => {
                    self.flags |= CellFlags::UNDERLINE;
                    self.underline_style = UnderlineStyle::Double;
                }
                22 => self.flags.remove(CellFlags::BOLD | CellFlags::DIM),
                23 => self.flags.remove(CellFlags::ITALIC),
                24 => {
                    self.flags.remove(CellFlags::UNDERLINE);
                    self.underline_style = UnderlineStyle::Single;
                }
                25 => self.flags.remove(CellFlags::BLINK),
                27 => self.flags.remove(CellFlags::INVERSE),
                28 => self.flags.remove(CellFlags::HIDDEN),
                29 => self.flags.remove(CellFlags::STRIKETHROUGH),
                30..=37 => self.fg = Color::Indexed((parameter - 30) as u8),
                39 => self.fg = Color::Default,
                40..=47 => self.bg = Color::Indexed((parameter - 40) as u8),
                49 => self.bg = Color::Default,
                90..=97 => self.fg = Color::Indexed((parameter - 90 + 8) as u8),
                100..=107 => self.bg = Color::Indexed((parameter - 100 + 8) as u8),
                38 => {
                    if let Some(color) = parse_ext_color(slice, &mut iter) {
                        self.fg = color;
                    }
                }
                48 => {
                    if let Some(color) = parse_ext_color(slice, &mut iter) {
                        self.bg = color;
                    }
                }
                58 => {
                    if let Some(color) = parse_ext_color(slice, &mut iter) {
                        // Malformed colors preserve the prior underline color, matching foreground and background.
                        self.underline_color = Some(color);
                    }
                }
                59 => self.underline_color = None,
                _ => {} // unknown — silently ignore for forward compat
            }
        }
    }
}

#[cfg(test)]
#[path = "vt_performer_tests.rs"]
mod vt_performer_tests;
