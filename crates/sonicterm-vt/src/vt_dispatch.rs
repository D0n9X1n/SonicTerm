use super::*;

impl Perform for Performer {
    fn print(&mut self, character: char) {
        self.print_graphic(character);
        self.fast_path_ready = true;
    }

    fn execute(&mut self, byte: u8) {
        self.reset_last_printed_char();
        match byte {
            0x07 => self.events.push(VtEvent::Bell),
            0x08 => self.grid.backspace(),
            0x09 => self.grid.tab(),
            0x0A..=0x0C => self.advance_hard_line(self.erase_fill_cell(), false),
            0x0D => self.grid.carriage_return(),
            _ => {
                // When: byte is an unhandled C0 control, leave cells unchanged while keeping the parser outside ground.
            }
        }
        // NB: do NOT set ground=true here. vte may call execute() while still
        // inside an ESC/CSI/OSC/DCS state machine (C0 bytes are dispatched
        // even mid-escape). Resuming the SWAR fast-path here would consume
        // the remainder of the escape sequence as printable text.
        self.fast_path_ready = false;
    }

    fn csi_dispatch(&mut self, params: &Params, inter: &[u8], _ignore: bool, action: char) {
        self.fast_path_ready = false;
        if action != 'b' {
            self.reset_last_printed_char();
        }
        // When: inter begins with ?, route private operations without falling through to ordinary CSI actions.
        if inter.first() == Some(&b'?') {
            match action {
                'h' => {
                    // When: action requests DECSET, apply private modes and finish this CSI before normal dispatch.
                    self.handle_dec_private_mode(params, true);
                    return;
                }
                'l' => {
                    // When: action requests DECRST, clear private modes and finish this CSI before normal dispatch.
                    self.handle_dec_private_mode(params, false);
                    return;
                }
                'u' => {
                    // When: action requests a Kitty query, reply with active flags and finish this CSI exactly once.
                    // Kitty keyboard protocol query: report the active flags.
                    let flags = self.kitty_keyboard_flags();
                    self.reply(format!("\x1b[?{flags}u").as_bytes());
                    return;
                }
                'p' if inter == b"?$" => {
                    // When: action is DECRQM; only mode 2026 is answered, every other mode stays silent as before.
                    let mut modes = params.iter();
                    if let (Some([2026]), None) = (modes.next(), modes.next()) {
                        let state = if self.synchronized_output { 1 } else { 2 };
                        self.reply(format!("\x1b[?2026;{state}$y").as_bytes());
                    }
                    return;
                }
                _ => {
                    // When: action has no private handler, consume it here so it cannot be misread as ordinary CSI.
                    return;
                }
            }
        }
        let first_param =
            || params.iter().next().and_then(|slice| slice.first().copied()).unwrap_or(0);
        let second_param =
            || params.iter().nth(1).and_then(|slice| slice.first().copied()).unwrap_or(0);
        // CSI with `>` intermediate — secondary DA / XTVERSION.

        // When: inter begins with >, handle identity replies or Kitty stack pushes before ordinary CSI dispatch.
        if inter.first() == Some(&b'>') {
            match action {
                'c' => {
                    // Secondary DA: VT220 (1), firmware version 0, ROM 0.
                    self.reply(b"\x1b[>1;0;0c");
                }
                'q' => {
                    // XTVERSION: DCS > | kitty SonicTerm <version> ST
                    let mut buf = Vec::with_capacity(XTVERSION_REPLY.len() + 6);
                    buf.extend_from_slice(b"\x1bP>|");
                    buf.extend_from_slice(XTVERSION_REPLY.as_bytes());
                    buf.extend_from_slice(b"\x1b\\");
                    self.reply(&buf);
                }
                'u' => {
                    // Kitty keyboard protocol push: `CSI > flags u`. Push the
                    // requested flag set and evict the oldest entry at the cap.
                    let before = self.win32_input_eligible();
                    let stack = self.kitty_kbd_stack_mut();
                    if stack.len() == KITTY_KBD_STACK_MAX {
                        stack.remove(0);
                    }
                    stack.push((first_param() & 0x7f) as u8);
                    self.record_win32_eligibility_transition(before);
                }
                'm' if first_param() == 4 => {
                    self.modify_other_keys = (second_param() as u8).min(2);
                }
                _ => {
                    // When: action is unsupported for >, ignore it without mutating identity or keyboard state.
                }
            }
            return;
        }
        // CSI with `<` intermediate — kitty keyboard protocol pop.
        // `CSI < number u` pops up to `number` (default 1) entries off the
        // flag stack. Other `<`-prefixed sequences are not used by SonicTerm.

        // When: inter begins with <, consume the keyboard-pop namespace without treating it as a normal CSI action.
        if inter.first() == Some(&b'<') {
            if action == 'u' {
                let before = self.win32_input_eligible();
                let count = (first_param() as usize).max(1);
                let stack = self.kitty_kbd_stack_mut();
                let new_len = stack.len().saturating_sub(count);
                stack.truncate(new_len);
                self.record_win32_eligibility_transition(before);
            }
            return;
        }
        // CSI with `=` intermediate — kitty keyboard protocol set.
        // `CSI = flags ; mode u` sets the current (top-of-stack) flags. `mode`
        // selects all (1)/set-or (2)/reset-and (3), with an omitted mode
        // defaulting to 1. Unsupported modes are ignored. With an empty stack,
        // an accepted mode pushes the requested flags as the active set.

        // When: inter begins with =, consume the keyboard-set namespace before normal CSI action routing.
        if inter.first() == Some(&b'=') {
            if action == 'u' {
                // When: action is u, apply the accepted Kitty set mode to this screen's stack.
                let before = self.win32_input_eligible();
                let flags = (first_param() & 0x7f) as u8;
                let mode =
                    params.iter().nth(1).and_then(|slice| slice.first().copied()).unwrap_or(1);
                let current = self.kitty_keyboard_flags();
                let next = match mode {
                    1 => flags,
                    2 => current | flags,
                    3 => current & !flags,
                    _ => {
                        // When: mode is outside 1..=3, leave the active Kitty flags unchanged.
                        return;
                    }
                };
                let stack = self.kitty_kbd_stack_mut();
                if let Some(top) = stack.last_mut() {
                    *top = next;
                } else {
                    // When: the active screen has no stack entry, establish its
                    // first explicit Kitty mode value.
                    stack.push(next);
                }
                self.record_win32_eligibility_transition(before);
            }
            return;
        }
        match action {
            'h' | 'l' => {
                let set = action == 'h';
                for slice in params.iter() {
                    if slice.first().copied().unwrap_or(0) == 20 {
                        self.newline_mode = set;
                    }
                }
            }
            'A' => {
                let count = first_param().max(1);
                let row = self.grid.cursor.row.saturating_sub(count);
                let col = self.grid.cursor.col;
                self.grid.goto(row, col);
            }
            'B' => {
                let count = first_param().max(1);
                let row = (self.grid.cursor.row + count).min(self.grid.rows.saturating_sub(1));
                let col = self.grid.cursor.col;
                self.grid.goto(row, col);
            }
            'C' => {
                let count = first_param().max(1);
                let row = self.grid.cursor.row;
                let col = (self.grid.cursor.col + count).min(self.grid.cols.saturating_sub(1));
                self.grid.goto(row, col);
            }
            'D' => {
                let count = first_param().max(1);
                let row = self.grid.cursor.row;
                let col = self.grid.cursor.col.saturating_sub(count);
                self.grid.goto(row, col);
            }
            'E' => {
                let count = first_param().max(1);
                let row = (self.grid.cursor.row + count).min(self.grid.rows.saturating_sub(1));
                self.grid.goto(row, 0);
            }
            'F' => {
                let count = first_param().max(1);
                let row = self.grid.cursor.row.saturating_sub(count);
                self.grid.goto(row, 0);
            }
            'H' | 'f' => {
                let row = first_param().saturating_sub(1);
                let col = second_param().saturating_sub(1);
                self.grid.goto(row, col);
            }
            'J' => {
                let mode = first_param();
                let (cursor_row, cursor_col) = (self.grid.cursor.row, self.grid.cursor.col);
                let (rows, cols) = (self.grid.rows, self.grid.cols);
                let will_blank = match mode {
                    0 => format!(
                        "rows ({cursor_row},{cursor_col})..({cursor_row},{}) + ({},0)..({},{})",
                        cols.saturating_sub(1),
                        cursor_row + 1,
                        rows.saturating_sub(1),
                        cols.saturating_sub(1)
                    ),
                    1 => format!("(0,0)..({cursor_row},{cursor_col}) inclusive"),
                    2 => "entire screen".to_string(),
                    3 if self.grid.is_alt() => "<alternate screen, no-op>".to_string(),
                    3 => "saved history only".to_string(),
                    _ => "<unknown mode, no-op>".to_string(),
                };
                tracing::debug!(
                    target: "sonicterm_vt::erase",
                    "CSI {mode}J: cursor=({cursor_row},{cursor_col}), grid_size=({rows},{cols}), will_blank={will_blank}"
                );
                match mode {
                    0 => self.grid.erase_below_with(self.erase_fill_cell()),
                    1 => self.grid.erase_above_with(self.erase_fill_cell()),
                    2 => self.grid.erase_screen_with(self.erase_fill_cell()),
                    3 => self.grid.clear_scrollback(),
                    _ => {
                        // When: mode is not a defined ED operation, preserve every cell instead of erasing an unintended region.
                    }
                }
            }
            'K' => {
                let mode = first_param();
                let (cursor_row, cursor_col) = (self.grid.cursor.row, self.grid.cursor.col);
                let (rows, cols) = (self.grid.rows, self.grid.cols);
                let will_blank = match mode {
                    0 => format!(
                        "cells ({cursor_row},{cursor_col})..({cursor_row},{})",
                        cols.saturating_sub(1)
                    ),
                    1 => format!("cells ({cursor_row},0)..({cursor_row},{cursor_col}) inclusive"),
                    2 => {
                        format!("cells ({cursor_row},0)..({cursor_row},{})", cols.saturating_sub(1))
                    }
                    _ => "<unknown mode, no-op>".to_string(),
                };
                tracing::debug!(
                    target: "sonicterm_vt::erase",
                    "CSI {mode}K: cursor=({cursor_row},{cursor_col}), grid_size=({rows},{cols}), will_blank={will_blank}"
                );
                match mode {
                    0 => self.grid.erase_line_to_end_with(self.erase_fill_cell()),
                    1 => self.grid.erase_line_to_start_with(self.erase_fill_cell()),
                    2 => self.grid.erase_line_with(self.erase_fill_cell()),
                    _ => {
                        // When: mode is not a defined EL operation, preserve the row instead of blanking cells the sender did not select.
                    }
                }
            }
            'L' => {
                // CSI Ps L — IL (Insert Line). Insert `count` blank lines at the
                // cursor row, pushing the rest of the scroll region down.
                // ECMA-48: no-op when cursor is outside the active region.
                // xterm behaviour: cursor moves to column 0.
                let count = first_param().max(1);
                let (top, bot) = self.effective_scroll_region();
                let cur = self.grid.cursor.row;
                if cur >= top && cur <= bot {
                    self.grid.scroll_region_down_with(cur, bot, count, self.erase_fill_cell());
                    self.grid.cursor.col = 0;
                }
            }
            'M' => {
                // CSI Ps M — DL (Delete Line). Delete `count` lines starting at
                // the cursor row, pulling the region below up. Cursor->col 0.
                let count = first_param().max(1);
                let (top, bot) = self.effective_scroll_region();
                let cur = self.grid.cursor.row;
                if cur >= top && cur <= bot {
                    self.grid.scroll_region_up_with(cur, bot, count, self.erase_fill_cell());
                    self.grid.cursor.col = 0;
                }
            }
            'm' => self.apply_sgr(params),
            '@' => {
                // ICH — Insert `count` blank cells at the cursor on the current
                // row, shifting trailing cells right and dropping overflow.
                let count = first_param().max(1) as usize;
                let cur = self.grid.cursor;
                self.grid.insert_cells_with(cur.row, cur.col, count, self.erase_fill_cell());
            }
            'P' => {
                // DCH — Delete `count` cells at the cursor, shifting trailing
                // cells left and filling the right edge with blanks.
                let count = first_param().max(1) as usize;
                let cur = self.grid.cursor;
                self.grid.delete_cells_with(cur.row, cur.col, count, self.erase_fill_cell());
            }
            'X' => {
                // ECH — Erase `count` cells starting at the cursor with the
                // current SGR blank cell. Cursor is unchanged. neo-tree's
                // per-row tail-clear pattern depends on this.
                let count = first_param().max(1) as usize;
                let cur = self.grid.cursor;
                self.grid.erase_cells_with(cur.row, cur.col, count, self.erase_fill_cell());
            }
            'G' | '`' => {
                // CHA (G) / HPA (`) — Cursor to column first_param() (1-based) on the
                // current row.
                let col_1 = first_param().max(1);
                let row = self.grid.cursor.row;
                self.grid.goto(row, col_1.saturating_sub(1));
            }
            'd' => {
                // VPA — Cursor to row `first_param()` (1-based), column unchanged.
                let row_1 = first_param().max(1);
                let col = self.grid.cursor.col;
                self.grid.goto(row_1.saturating_sub(1), col);
            }
            'b' => {
                // REP — Repeat last printable character `count` times at cursor.
                let count = first_param().max(1) as usize;
                if let Some(character) = self.last_printed_char {
                    for _ in 0..count {
                        self.print_graphic(character);
                    }
                }
            }
            'n' => {
                // When: action requests DSR, answer only supported first_param() queries so no terminal status is fabricated.
                match first_param() {
                    5 => self.reply(b"\x1b[0n"),
                    6 => {
                        let row = self.grid.cursor.row.saturating_add(1);
                        let col = self.physical_cursor_col().saturating_add(1);
                        self.reply(format!("\x1b[{row};{col}R").as_bytes());
                    }
                    _ => {}
                }
            }
            'c' => {
                // Primary DA — VT220 with 132-columns (62) + printer port (c).
                let parameter = first_param();
                if parameter == 0 {
                    self.reply(b"\x1b[?62;c");
                }
            }
            'S' => {
                // CSI Ps S — Scroll Up (SU). Scrolls the active region
                // up by `count` lines, fills bottom with blanks. Dest rows
                // are marked dirty by the grid, which is the fix for
                // (stale LineQuadCache entries after region scroll).
                let count = first_param().max(1);
                let (top, bot) = self.effective_scroll_region();
                self.grid.scroll_region_up_with(top, bot, count, self.erase_fill_cell());
            }
            'T' => {
                // CSI Ps T — Scroll Down (SD).
                let count = first_param().max(1);
                let (top, bot) = self.effective_scroll_region();
                self.grid.scroll_region_down_with(top, bot, count, self.erase_fill_cell());
            }
            'r' => {
                // CSI Ps ; Ps r — DECSTBM Set Top and Bottom Margins.
                // Both omitted / 0 / out-of-range -> reset to full
                // screen. Cursor moves to home as per spec.
                let rows = self.grid.rows;
                let top_p = first_param();
                let bot_p = second_param();
                let cur_before = (self.grid.cursor.row, self.grid.cursor.col);
                let new_top = if top_p == 0 {
                    0
                } else {
                    // When: top_p is explicit, convert its one-based DEC coordinate to the grid's zero-based row.
                    top_p.saturating_sub(1)
                };
                let new_bot = if bot_p == 0 {
                    rows.saturating_sub(1)
                } else {
                    // When: bot_p is explicit, convert its one-based DEC coordinate to the grid's zero-based row.
                    bot_p.saturating_sub(1)
                };
                let (applied_top, applied_bot) = if new_top < new_bot && new_bot < rows {
                    self.scroll_top = Some(new_top);
                    self.scroll_bottom = Some(new_bot);
                    (new_top, new_bot)
                } else {
                    // When: new_top and new_bot do not form an ordered region within rows, reset DECSTBM to the full screen.
                    self.scroll_top = None;
                    self.scroll_bottom = None;
                    (0, rows.saturating_sub(1))
                };
                self.grid.goto(0, 0);
                tracing::debug!(
                    target: "sonicterm_vt::stbm",
                    "CSI {top_p};{bot_p}r DECSTBM: parsed=({new_top},{new_bot}), applied=({applied_top},{applied_bot}), grid_rows={rows}, cursor {:?}→(0,0)",
                    cur_before
                );
            }
            _ => {
                // When: action is unsupported, complete the CSI without moving the cursor or changing styled grid cells.
            }
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool) {
        self.fast_path_ready = false;
        let code = params
            .first()
            .and_then(|param| std::str::from_utf8(param).ok())
            .and_then(|text| text.parse::<u16>().ok());
        // When: code selects a supported OSC contract; unknown codes complete without mutating terminal state.
        match code {
            Some(0) | Some(2) => {
                if let Some(text) = params.get(1).and_then(|param| std::str::from_utf8(param).ok())
                {
                    self.title = Some(text.to_string());
                    self.events.push(VtEvent::SetTitle(text.to_string()));
                }
            }
            Some(7) => {
                // OSC 7 ; file://<host>/<path> ST — shell-reported cwd.
                // Used by the tab-title renderer to show `parent/leaf`.
                // We are permissive: accept the raw payload even when it
                // doesn't start with `file://` (some shells skip the
                // scheme), strip the host component when present, and
                // percent-decode the path so spaces/unicode survive.
                if let Some(raw) = params.get(1).and_then(|param| std::str::from_utf8(param).ok()) {
                    self.cwd_revision = self.cwd_revision.wrapping_add(1);
                    self.osc7_cwd = parse_osc7_cwd_snapshot(raw);
                    let title_path = parse_osc7_cwd(raw);
                    if !title_path.is_empty() {
                        self.cwd = Some(title_path);
                    }
                }
            }
            Some(8) => {
                // OSC 8;params;uri ST — hyperlink. Empty uri = end of link.
                let id = params.get(1).and_then(|param| std::str::from_utf8(param).ok());
                let uri = params.get(2).and_then(|param| std::str::from_utf8(param).ok());
                if let Some(uri) = uri {
                    let id_norm = id
                        .and_then(|parameters| {
                            parameters.split(':').find_map(|part| part.strip_prefix("id="))
                        })
                        .filter(|value| !value.is_empty());
                    if uri.is_empty() {
                        self.current_hyperlink = None;
                        self.events.push(VtEvent::Hyperlink {
                            id: id_norm
                                .filter(|value| value.len() <= MAX_HYPERLINK_CLIENT_ID_BYTES)
                                .map(String::from),
                            uri: String::new(),
                        });
                    } else {
                        // When: uri is non-empty, intern it before assigning cells so rejected links remain plain text rather than dangling ids.
                        let mut admission = self.hyperlinks.intern_or_reject(id_norm, uri);
                        // Only a rejection that reclamation could plausibly
                        // relieve is worth a grid scan. An oversized URI is
                        // refused by a size check no sweep can change, so
                        // retrying it burns an O(visible + scrollback) walk to
                        // reach the same answer — once per link, for as long
                        // as the shell keeps emitting them.
                        if admission
                            .as_ref()
                            .err()
                            .is_some_and(|reason| reason.is_retryable_after_reclaim())
                        {
                            // The registry is full. Almost all of it is links
                            // whose cells scrolled away, so reclaim and retry
                            // rather than leaving this and every later link
                            // dead for the rest of the session.
                            //
                            // Swept before `current_hyperlink` is reassigned,
                            // so the span the parser still considers open is a
                            // live root even though no cell holds it yet.
                            //
                            // Backoff bounds the scan: when the links really
                            // are all still on screen the sweep frees nothing,
                            // and retrying per link would scan the whole grid
                            // per link.
                            //
                            // It is released as soon as scrollback evicts a
                            // row, because that is the event that turns live
                            // links into garbage. A blind countdown outlived
                            // the state it was set on: scrolling every link
                            // out of history made the whole registry
                            // reclaimable while the next links still took the
                            // skip branch and rendered as plain text.
                            let evicted = self.grid.scrollback_evicted();
                            if evicted != self.hyperlink_reclaim_scanned_at {
                                self.hyperlink_reclaim_backoff = 0;
                            }
                            if self.hyperlink_reclaim_backoff == 0 {
                                let freed = self.reclaim_hyperlinks();
                                self.hyperlink_reclaim_scanned_at = evicted;
                                self.hyperlink_reclaim_backoff =
                                    if freed > 0 { 0 } else { HYPERLINK_RECLAIM_BACKOFF_LINKS };
                                if freed > 0 {
                                    tracing::debug!(
                                        target: "memory",
                                        freed,
                                        retained = self.hyperlinks.len(),
                                        retained_bytes = self.hyperlinks.retained_bytes(),
                                        "reclaimed unreferenced hyperlinks"
                                    );
                                    admission = self.hyperlinks.intern_or_reject(id_norm, uri);
                                }
                            } else {
                                // When: hyperlink_reclaim_backoff remains active, skip the full-grid scan that just proved unproductive.
                                self.hyperlink_reclaim_backoff -= 1;
                            }
                        }
                        self.current_hyperlink = admission.as_ref().ok().copied();
                        if self.current_hyperlink.is_some() {
                            self.events.push(VtEvent::Hyperlink {
                                id: id_norm.map(String::from),
                                uri: uri.to_string(),
                            });
                        } else {
                            // When: current_hyperlink is absent after admission, emit closure so following cells cannot inherit a rejected URI.
                            self.events.push(VtEvent::Hyperlink { id: None, uri: String::new() });
                            tracing::warn!(
                                target: "memory",
                                uri_bytes = uri.len(),
                                id_bytes = id_norm.map_or(0, str::len),
                                retained = self.hyperlinks.len(),
                                reason = admission
                                    .as_ref()
                                    .err()
                                    .map_or("unknown", |reason| reason.code()),
                                "OSC 8 hyperlink rejected by memory limits"
                            );
                        }
                    }
                }
            }
            Some(4) => {
                // OSC 4 ; <index> ; ? ST — query a palette colour. xterm
                // allows multiple `index ; spec` pairs in one OSC 4, so we
                // walk the params two at a time. A `?` spec is a query → reply
                // `ESC ] 4 ; <index> ; rgb:RRRR/GGGG/BBBB ST` (16-bit channels,
                // xterm canonical). A non-`?` spec would be a *set*, which we
                // don't implement yet (theme owns the palette) — skip it.
                //
                // Some CLIs (GitHub Copilot's prompt frame) gate their richer
                // UI on being able to read backgroundSecondary via the full
                // OSC 4 palette + OSC 10/11 set; without these replies they
                // treat SonicTerm as colourless and disable the frame.
                //
                // vte 0.15 keeps the full OSC payload in a private buffer but
                // exposes only MAX_OSC_PARAMS split params here. Parser::advance
                // therefore captures raw OSC 4 bytes in parallel and handles
                // full 16-colour batch queries before this capped callback. Keep
                // this path as a fallback for synthetic/direct Performer tests
                // for any capture miss, but avoid duplicate replies.
                if self.suppress_next_osc4 {
                    // When: suppress_next_osc4 marks a query already answered from raw bytes, avoid sending a duplicate palette reply.
                    self.suppress_next_osc4 = false;
                    return;
                }
                self.handle_osc4_pairs(params, bell_terminated);
            }
            Some(code @ 10..=12) => {
                // OSC 10/11/12 ; ? ST — query default fg/bg/cursor colour.
                // Reply format (xterm): `ESC ] N ; rgb:RRRR/GGGG/BBBB ST`
                // where each channel is duplicated to 16 bits (xterm
                // canonical form, accepted by every consumer including
                // nvim). Terminator matches the request's terminator
                // (BEL → BEL, ST → ST) so we don't surprise the client.
                //
                // Without this reply nvim falls back to a hard-coded
                // guess for the bg (NeoTreeNormal 27,29,30), which
                // doesn't match SonicTerm's actual theme bg — neo-tree
                // icon cells (painted with `bg=NONE`) then visibly
                // differ from the surrounding theme-clear surface.
                // OSC 10/11/12 *set* (payload is a colour, not `?`)
                // is intentionally not implemented yet — diagnosis
                // shows query-reply is sufficient to fix.
                let payload = params.get(1).and_then(|param| std::str::from_utf8(param).ok());
                if payload != Some("?") {
                    // When: payload is not a query marker, ignore unsupported OSC color-setting input rather than changing the theme.
                    return;
                }
                let rgb = match code {
                    10 => self.theme_fg,
                    11 => self.theme_bg,
                    12 => self.theme_cursor.or(self.theme_fg),
                    _ => None,
                };
                let Some((red, green, blue)) = rgb else {
                    // When: rgb is unavailable, omit the reply rather than reporting a color the host never supplied.
                    return;
                };
                let terminator: &[u8] = if bell_terminated { b"\x07" } else { b"\x1b\\" };
                let mut buf = Vec::with_capacity(24);
                buf.extend_from_slice(b"\x1b]");
                buf.extend_from_slice(code.to_string().as_bytes());
                buf.extend_from_slice(
                    format!(";rgb:{red:02x}{red:02x}/{green:02x}{green:02x}/{blue:02x}{blue:02x}")
                        .as_bytes(),
                );
                buf.extend_from_slice(terminator);
                self.reply(&buf);
            }
            Some(9) | Some(777) => {
                // Raw capture owns every OSC 9 and 777 it recognizes; one that reaches vte has a
                // spelling it missed (a digit run past the probe, a C1 introducer) and is dropped.
            }
            Some(52) => {
                let sel =
                    params.get(1).and_then(|param| param.first().copied()).unwrap_or(b'c') as char;
                let data = params
                    .get(2)
                    .and_then(|param| std::str::from_utf8(param).ok())
                    .unwrap_or_default()
                    .to_string();
                self.events.push(VtEvent::Clipboard { selection: sel, data });
            }
            Some(1337) => {
                let payload = join_osc_params(params.get(1..).unwrap_or(&[]));
                let row = self.grid.cursor.row;
                let col = self.grid.cursor.col;
                if let Some(event) = parse_iterm2_file_event(payload.as_bytes(), row, col) {
                    self.push_media(event);
                }
            }
            Some(133) => {
                // OSC 133 ; <kind> [; <args>] ST — FinalTerm/WezTerm shell
                // integration. Kinds:
                //   A → prompt start
                //   B → prompt end / editable input start
                //   C → command output start
                //   D [; exit_code] → command finished
                let kind = params.get(1).and_then(|param| param.first().copied());
                match kind {
                    Some(b'A') => {
                        self.grid.record_prompt_start();
                        self.events.push(VtEvent::Command(CommandEvent::PromptStart));
                    }
                    Some(b'B') => {
                        self.events.push(VtEvent::Command(CommandEvent::PromptEnd));
                    }
                    Some(b'C') => {
                        self.events.push(VtEvent::Command(CommandEvent::CmdStart));
                    }
                    Some(b'D') => {
                        let exit_i32 = params
                            .get(2)
                            .and_then(|param| std::str::from_utf8(param).ok())
                            .and_then(|text| text.parse::<i32>().ok());
                        self.grid.record_prompt_end(exit_i32);
                        let exit = exit_i32.and_then(|status| u8::try_from(status).ok());
                        self.events.push(VtEvent::Command(CommandEvent::CmdEnd(exit)));
                    }
                    _ => {
                        // When: kind is not a recognized shell marker, preserve prompt boundaries and emit no command event.
                    }
                }
            }
            _ => {}
        }
    }

    fn hook(&mut self, params: &Params, intermediates: &[u8], ignore: bool, action: char) {
        // Entering DCS passthrough — stay out of the fast-path until unhook.
        self.fast_path_ready = false;
        self.clear_decrqss();
        if action == 'q' && intermediates == b"$" && !ignore {
            // Classify status selectors independently of media capture.
            let mut header = params.iter();
            let default_params =
                header.next().is_none_or(|value| value == [0]) && header.next().is_none();
            self.decrqss = Some(if default_params {
                DecrqssRequest::Empty
            } else {
                DecrqssRequest::Unsupported
            });
        }
        // `q` ends three unrelated DCS sequences, told apart only by their
        // intermediate byte:
        //
        //   Sixel      DCS <params> q   no intermediate
        //   DECRQSS    DCS $ q          intermediate 0x24
        //   XTGETTCAP  DCS + q          intermediate 0x2B
        //
        // Only the first is image data. Matching on the final byte alone
        // captures a terminal *query* and hands its reply to the Sixel
        // decoder, which renders the bytes as pixels: a DECRQSS reply of `m`
        // decodes to a 1x6 column in the decoder's default palette entry,
        // which is pure white. That reaches the screen as a stray white mark
        // at the pane content origin, redrawn every frame from retained
        // media and surviving every repaint path.
        // `ignore` is set when the sequence exceeded the parser's parameter or
        // intermediate limits, so what arrives is a truncated view of
        // something longer. Capturing that as an image would decode bytes the
        // sender never meant as pixel data.
        let sixel = action == 'q' && intermediates.is_empty() && !ignore;
        // An open kitty upload releases its reservation before the Sixel capture asks for one,
        // so one parser never holds two and the Sixel is not refused for the upload's sake.
        if sixel {
            self.abort_kitty_transfer_for_other_media();
        }
        self.dcs_capture = sixel
            .then(|| MediaCapture::new(&self.staging_pool, MediaProtocol::Sixel, String::new()));
    }
    fn put(&mut self, byte: u8) {
        self.fast_path_ready = false;
        if let Some(request) = self.decrqss.as_mut() {
            // Only exactly one m selects the SGR response; other payloads stay unsupported.
            *request = match (*request, byte) {
                (DecrqssRequest::Empty, b'm') => DecrqssRequest::Sgr,
                _ => DecrqssRequest::Unsupported,
            };
        }
        if let Some(capture) = self.dcs_capture.as_mut() {
            capture.append_byte(byte);
        }
    }
    fn unhook(&mut self) {
        self.fast_path_ready = false;
        if let Some(capture) = self.dcs_capture.take() {
            if let Some(event) = capture.into_event(self.grid.cursor.row, self.grid.cursor.col) {
                self.push_media(event);
            }
        }
    }
    fn esc_dispatch(&mut self, _intermediates: &[u8], _ignore: bool, byte: u8) {
        self.fast_path_ready = false;
        self.reset_last_printed_char();
        match byte {
            b'7' => {
                // DECSC — save cursor. Claude Code uses ESC 7 / ESC 8
                // around DECSTBM reset at startup; without this, CSI r
                // leaves the cursor at home and the trust prompt paints
                // over old scrollback instead of starting below the shell
                // prompt.
                self.saved_cursor = Some(self.grid.cursor);
            }
            b'8' => {
                // DECRC — restore cursor saved by DECSC / ?1048.
                if let Some(saved) = self.saved_cursor {
                    self.grid.goto(saved.row, saved.col);
                }
            }
            b'c' => {
                // RIS — Reset to Initial State. TUI launchers such as
                // Claude Code may use this as their first "clean slate"
                // before painting. Ignoring it leaves shell scrollback
                // visually interleaved with the app's first frame.
                self.reset_terminal();
            }
            b'=' => {
                self.app_keypad = true;
            }
            b'>' => {
                self.app_keypad = false;
            }
            b'D' => self.advance_hard_line(Cell::default(), false),
            b'M' => {
                // RI — Reverse Index. Move cursor up; if at top
                // margin, scroll the region down.
                let (top, bot) = self.effective_scroll_region();
                if self.grid.cursor.row == top {
                    self.grid.scroll_region_down(top, bot, 1);
                } else {
                    // When: cursor row is not top, RI moves upward without scrolling cells in the active region.
                    let new_row = self.grid.cursor.row.saturating_sub(1);
                    let col = self.grid.cursor.col;
                    self.grid.goto(new_row, col);
                }
            }
            b'E' => self.advance_hard_line(Cell::default(), true),
            _ => {
                // When: byte is not a supported ESC final, leave cursor and screen state unchanged.
            }
        }
    }
}

fn join_osc_params(params: &[&[u8]]) -> String {
    let mut out = Vec::new();
    for (idx, param) in params.iter().enumerate() {
        if idx > 0 {
            out.push(b';');
        }
        out.extend_from_slice(param);
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
#[path = "vt_dispatch_tests.rs"]
mod vt_dispatch_tests;
