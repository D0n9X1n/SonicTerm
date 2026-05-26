//! VT/ANSI parser. We delegate the lexer to the `vte` crate (the same
//! implementation alacritty uses) and translate parsed events into mutations
//! on a [`crate::grid::Grid`].
//!
//! The supported subset (v0.1.0):
//! - Printable ASCII + UTF-8
//! - C0 controls: BEL, BS, HT, LF, CR
//! - CSI: `H`/`f` (CUP), `A`/`B`/`C`/`D` (cursor motion), `J` (ED), `K` (EL),
//!   `m` (SGR — bold/italic/underline/inverse/reset + 30..37, 40..47, 90..97,
//!   100..107, 38;5;n / 48;5;n, 38;2;r;g;b / 48;2;r;g;b)
//! - OSC: `0`/`2` (window title), `8` (hyperlink), `52` (clipboard — stub)
//!
//! Out of scope: DEC private modes (most), Sixel, Kitty graphics, mouse
//! tracking. These will be added in follow-up PRs.

use crossbeam_channel::Sender;
use vte::{Params, Perform};

use crate::grid::{CellFlags, Color, Grid, Pos};
use crate::hyperlink::{HyperlinkId, HyperlinkRegistry};

/// Version string reported in answer to CSI > q (XTVERSION).
pub const SONIC_VERSION: &str = "Sonic 0.7";

/// Event surfaced to the host so it can update window chrome, clipboard, etc.
#[derive(Debug, Clone)]
pub enum VtEvent {
    SetTitle(String),
    Bell,
    Hyperlink {
        id: Option<String>,
        uri: String,
    },
    Clipboard {
        selection: char,
        data: String,
    },
    /// DEC private mode ?25 — host should show/hide the cursor.
    CursorVisibility(bool),
}

/// Streaming parser wrapping `vte::Parser` and a [`Performer`] that owns the
/// grid + current SGR attributes.
pub struct Parser {
    inner: vte::Parser,
    performer: Performer,
}

impl Parser {
    pub fn new(grid: Grid) -> Self {
        Self { inner: vte::Parser::new(), performer: Performer::new(grid, None) }
    }

    /// Construct a parser that can send replies (DSR, DA, XTVERSION, focus
    /// reporting) back to the pty via the given channel.
    pub fn new_with_reply(grid: Grid, reply_tx: Sender<Vec<u8>>) -> Self {
        Self { inner: vte::Parser::new(), performer: Performer::new(grid, Some(reply_tx)) }
    }

    /// Whether DECSET ?1004 (focus reporting) is currently enabled. App should
    /// send `\e[I` / `\e[O` on focus in/out when this is true.
    pub fn focus_reporting_enabled(&self) -> bool {
        self.performer.focus_reporting
    }

    /// Feed raw bytes from the pty. Drains any queued events for the caller.
    pub fn advance(&mut self, bytes: &[u8]) -> Vec<VtEvent> {
        self.inner.advance(&mut self.performer, bytes);
        std::mem::take(&mut self.performer.events)
    }

    pub fn grid(&self) -> &Grid {
        &self.performer.grid
    }

    pub fn grid_mut(&mut self) -> &mut Grid {
        &mut self.performer.grid
    }

    /// Borrow the hyperlink registry (OSC 8 interned uris).
    pub fn hyperlinks(&self) -> &HyperlinkRegistry {
        &self.performer.hyperlinks
    }

    /// Currently-active hyperlink id, if any.
    pub fn current_hyperlink(&self) -> Option<HyperlinkId> {
        self.performer.current_hyperlink
    }

    /// Whether DECSET ?2004 (bracketed paste) is currently enabled.
    pub fn bracketed_paste_enabled(&self) -> bool {
        self.performer.bracketed_paste
    }

    /// Whether DECSET ?1006 (SGR mouse reporting) is currently enabled.
    pub fn mouse_sgr_enabled(&self) -> bool {
        self.performer.mouse_sgr
    }

    /// Latest OSC 0/2 window title (sticky), or `None` if no title has been
    /// set. Used by the tab bar to label tabs with the shell's reported title.
    pub fn title(&self) -> Option<&str> {
        self.performer.title.as_deref()
    }
}

struct Performer {
    grid: Grid,
    fg: Color,
    bg: Color,
    flags: CellFlags,
    events: Vec<VtEvent>,
    hyperlinks: HyperlinkRegistry,
    current_hyperlink: Option<HyperlinkId>,
    /// Cursor saved by DECSET ?1049 when entering the alt screen.
    saved_cursor: Option<Pos>,
    bracketed_paste: bool,
    mouse_sgr: bool,
    focus_reporting: bool,
    /// Latest OSC 0/2 title (sticky — survives consumed events).
    title: Option<String>,
    reply_tx: Option<Sender<Vec<u8>>>,
}

impl Performer {
    fn new(grid: Grid, reply_tx: Option<Sender<Vec<u8>>>) -> Self {
        Self {
            grid,
            fg: Color::Default,
            bg: Color::Default,
            flags: CellFlags::empty(),
            events: Vec::new(),
            hyperlinks: HyperlinkRegistry::new(),
            current_hyperlink: None,
            saved_cursor: None,
            bracketed_paste: false,
            mouse_sgr: false,
            focus_reporting: false,
            title: None,
            reply_tx,
        }
    }

    fn reply(&self, bytes: &[u8]) {
        if let Some(tx) = &self.reply_tx {
            let _ = tx.send(bytes.to_vec());
        }
    }

    fn reset_attrs(&mut self) {
        self.fg = Color::Default;
        self.bg = Color::Default;
        self.flags = CellFlags::empty();
    }

    fn apply_sgr(&mut self, params: &Params) {
        let mut iter = params.iter();
        while let Some(slice) = iter.next() {
            let p = slice.first().copied().unwrap_or(0);
            match p {
                0 => self.reset_attrs(),
                1 => self.flags |= CellFlags::BOLD,
                2 => self.flags |= CellFlags::DIM,
                3 => self.flags |= CellFlags::ITALIC,
                4 => self.flags |= CellFlags::UNDERLINE,
                5 => self.flags |= CellFlags::BLINK,
                7 => self.flags |= CellFlags::INVERSE,
                8 => self.flags |= CellFlags::HIDDEN,
                9 => self.flags |= CellFlags::STRIKETHROUGH,
                22 => self.flags.remove(CellFlags::BOLD | CellFlags::DIM),
                23 => self.flags.remove(CellFlags::ITALIC),
                24 => self.flags.remove(CellFlags::UNDERLINE),
                25 => self.flags.remove(CellFlags::BLINK),
                27 => self.flags.remove(CellFlags::INVERSE),
                28 => self.flags.remove(CellFlags::HIDDEN),
                29 => self.flags.remove(CellFlags::STRIKETHROUGH),
                30..=37 => self.fg = Color::Indexed((p - 30) as u8),
                39 => self.fg = Color::Default,
                40..=47 => self.bg = Color::Indexed((p - 40) as u8),
                49 => self.bg = Color::Default,
                90..=97 => self.fg = Color::Indexed((p - 90 + 8) as u8),
                100..=107 => self.bg = Color::Indexed((p - 100 + 8) as u8),
                38 => {
                    if let Some(c) = parse_ext_color(&mut iter) {
                        self.fg = c;
                    }
                }
                48 => {
                    if let Some(c) = parse_ext_color(&mut iter) {
                        self.bg = c;
                    }
                }
                _ => {} // unknown — silently ignore for forward compat
            }
        }
    }

    /// Handle a CSI sequence with `?` intermediate (DEC private modes).
    fn handle_dec_private_mode(&mut self, params: &Params, set: bool) {
        for slice in params.iter() {
            let code = slice.first().copied().unwrap_or(0);
            match code {
                25 => self.events.push(VtEvent::CursorVisibility(set)),
                47 => {
                    if set {
                        self.grid.enter_alt_screen();
                    } else {
                        self.grid.leave_alt_screen();
                    }
                }
                1049 => {
                    if set {
                        // Guard against repeated ?1049h while already in alt
                        // screen — must not clobber the previously saved
                        // primary-screen cursor. xterm behaviour: second
                        // ?1049h is a no-op.
                        if !self.grid.is_alt() {
                            self.saved_cursor = Some(self.grid.cursor);
                            self.grid.enter_alt_screen();
                        }
                    } else {
                        self.grid.leave_alt_screen();
                        if let Some(c) = self.saved_cursor.take() {
                            self.grid.goto(c.row, c.col);
                        }
                    }
                }
                2004 => self.bracketed_paste = set,
                1006 => self.mouse_sgr = set,
                1004 => self.focus_reporting = set,
                _ => {}
            }
        }
    }
}

fn parse_ext_color(iter: &mut vte::ParamsIter<'_>) -> Option<Color> {
    let mode = iter.next()?.first().copied()?;
    match mode {
        5 => Some(Color::Indexed(iter.next()?.first().copied()? as u8)),
        2 => {
            let r = iter.next()?.first().copied()? as u8;
            let g = iter.next()?.first().copied()? as u8;
            let b = iter.next()?.first().copied()? as u8;
            Some(Color::Rgb(r, g, b))
        }
        _ => None,
    }
}

impl Perform for Performer {
    fn print(&mut self, c: char) {
        self.grid.put_char_linked(c, self.fg, self.bg, self.flags, self.current_hyperlink);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            0x07 => self.events.push(VtEvent::Bell),
            0x08 => self.grid.backspace(),
            0x09 => self.grid.tab(),
            0x0A..=0x0C => self.grid.linefeed(),
            0x0D => self.grid.carriage_return(),
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, inter: &[u8], _ignore: bool, action: char) {
        if inter.first() == Some(&b'?') {
            match action {
                'h' => {
                    self.handle_dec_private_mode(params, true);
                    return;
                }
                'l' => {
                    self.handle_dec_private_mode(params, false);
                    return;
                }
                _ => return,
            }
        }
        let p0 = || params.iter().next().and_then(|s| s.first().copied()).unwrap_or(0);
        let p1 = || params.iter().nth(1).and_then(|s| s.first().copied()).unwrap_or(0);
        // CSI with `>` intermediate — secondary DA / XTVERSION.
        if inter.first() == Some(&b'>') {
            match action {
                'c' => {
                    // Secondary DA: VT220 (1), firmware version 0, ROM 0.
                    self.reply(b"\x1b[>1;0;0c");
                }
                'q' => {
                    // XTVERSION: DCS > | <name> ST
                    let mut buf = Vec::with_capacity(SONIC_VERSION.len() + 5);
                    buf.extend_from_slice(b"\x1bP>|");
                    buf.extend_from_slice(SONIC_VERSION.as_bytes());
                    buf.extend_from_slice(b"\x1b\\");
                    self.reply(&buf);
                }
                _ => {}
            }
            return;
        }
        match action {
            'A' => {
                let n = p0().max(1);
                let row = self.grid.cursor.row.saturating_sub(n);
                let col = self.grid.cursor.col;
                self.grid.goto(row, col);
            }
            'B' => {
                let n = p0().max(1);
                let row = (self.grid.cursor.row + n).min(self.grid.rows.saturating_sub(1));
                let col = self.grid.cursor.col;
                self.grid.goto(row, col);
            }
            'C' => {
                let n = p0().max(1);
                let row = self.grid.cursor.row;
                let col = (self.grid.cursor.col + n).min(self.grid.cols.saturating_sub(1));
                self.grid.goto(row, col);
            }
            'D' => {
                let n = p0().max(1);
                let row = self.grid.cursor.row;
                let col = self.grid.cursor.col.saturating_sub(n);
                self.grid.goto(row, col);
            }
            'H' | 'f' => {
                let row = p0().saturating_sub(1);
                let col = p1().saturating_sub(1);
                self.grid.goto(row, col);
            }
            'J' => match p0() {
                0 => self.grid.erase_below(),
                1 => self.grid.erase_above(),
                2 | 3 => self.grid.erase_screen(),
                _ => {}
            },
            'K' => match p0() {
                0 => self.grid.erase_line_to_end(),
                1 => self.grid.erase_line_to_start(),
                2 => self.grid.erase_line(),
                _ => {}
            },
            'm' => self.apply_sgr(params),
            'n' => match p0() {
                5 => self.reply(b"\x1b[0n"),
                6 => {
                    let row = self.grid.cursor.row.saturating_add(1);
                    let col = self.grid.cursor.col.saturating_add(1);
                    self.reply(format!("\x1b[{row};{col}R").as_bytes());
                }
                _ => {}
            },
            'c' => {
                // Primary DA — VT220 with 132-columns (62) + printer port (c).
                let p = p0();
                if p == 0 {
                    self.reply(b"\x1b[?62;c");
                }
            }
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        let code = params
            .first()
            .and_then(|s| std::str::from_utf8(s).ok())
            .and_then(|s| s.parse::<u16>().ok());
        match code {
            Some(0) | Some(2) => {
                if let Some(text) = params.get(1).and_then(|s| std::str::from_utf8(s).ok()) {
                    self.title = Some(text.to_string());
                    self.events.push(VtEvent::SetTitle(text.to_string()));
                }
            }
            Some(8) => {
                // OSC 8;params;uri ST — hyperlink. Empty uri = end of link.
                let id = params.get(1).and_then(|s| std::str::from_utf8(s).ok());
                let uri = params.get(2).and_then(|s| std::str::from_utf8(s).ok());
                if let Some(uri) = uri {
                    let id_norm = id.filter(|s| !s.is_empty());
                    if uri.is_empty() {
                        self.current_hyperlink = None;
                    } else {
                        let hid = self.hyperlinks.intern(id_norm, uri);
                        self.current_hyperlink = Some(hid);
                    }
                    self.events.push(VtEvent::Hyperlink {
                        id: id_norm.map(String::from),
                        uri: uri.to_string(),
                    });
                }
            }
            Some(52) => {
                let sel = params.get(1).and_then(|s| s.first().copied()).unwrap_or(b'c') as char;
                let data = params
                    .get(2)
                    .and_then(|s| std::str::from_utf8(s).ok())
                    .unwrap_or_default()
                    .to_string();
                self.events.push(VtEvent::Clipboard { selection: sel, data });
            }
            Some(133) => {
                // OSC 133 ; <kind> [; <args>] ST — FinalTerm/WezTerm shell
                // integration. Kinds:
                //   A → prompt start
                //   B → prompt end (= command-line edit start)
                //   C → command output start
                //   D [; exit_code] → command finished
                let kind = params.get(1).and_then(|s| s.first().copied());
                match kind {
                    Some(b'A') => self.grid.record_prompt_start(),
                    Some(b'D') => {
                        let exit = params
                            .get(2)
                            .and_then(|s| std::str::from_utf8(s).ok())
                            .and_then(|s| s.parse::<i32>().ok());
                        self.grid.record_prompt_end(exit);
                    }
                    // B / C are tracked implicitly via cursor position at the
                    // time A and D fire; no extra state needed for now.
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn hook(&mut self, _: &Params, _: &[u8], _: bool, _: char) {}
    fn put(&mut self, _byte: u8) {}
    fn unhook(&mut self) {}
    fn esc_dispatch(&mut self, _intermediates: &[u8], _ignore: bool, _byte: u8) {}
}
