//! VT/ANSI parser. We delegate the lexer to the `vte` crate (the same
//! implementation alacritty uses) and translate parsed events into mutations
//! on a [`sonic_grid::grid::Grid`].
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

use sonic_grid::grid::{CellFlags, Color, Grid, Pos};
use sonic_grid::hyperlink::{HyperlinkId, HyperlinkRegistry};

/// Version string reported in answer to CSI > q (XTVERSION).
pub const SONIC_VERSION: &str = "Sonic 0.7";

/// Event surfaced to the host so it can update window chrome, clipboard, etc.
#[derive(Debug, Clone)]
pub enum VtEvent {
    /// OSC 133 — shell integration command lifecycle marker.
    Command(CommandEvent),
    /// OSC 0/2 — shell asked the terminal to update the window title.
    SetTitle(String),
    /// BEL (0x07) — audible/visual bell request from the shell.
    Bell,
    /// OSC 8 — enter (or exit, when `uri` is empty) a hyperlink span; cells
    /// emitted while active carry the interned id so the renderer can underline
    /// them and the input layer can resolve clicks back to a URI.
    Hyperlink {
        /// Optional `id=…` parameter so multiple discontiguous runs can share
        /// one logical link target.
        id: Option<String>,
        /// The target URI; empty string terminates the currently-active link.
        uri: String,
    },
    /// OSC 52 — shell requested clipboard read/write on the named selection.
    Clipboard {
        /// Selection target byte (`c` = clipboard, `p` = primary, etc.).
        selection: char,
        /// Base64-encoded payload as received from the shell.
        data: String,
    },
    /// DEC private mode ?25 — host should show/hide the cursor.
    CursorVisibility(bool),
}

/// Command lifecycle events surfaced from OSC 133 shell-integration markers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandEvent {
    /// Prompt started (`OSC 133 ; A`).
    PromptStart,
    /// Command started (`OSC 133 ; B` or `OSC 133 ; C`).
    CmdStart,
    /// Command ended, optionally with an exit code (`OSC 133 ; D ; <code>`).
    CmdEnd(Option<u8>),
}

/// Streaming parser wrapping `vte::Parser` and a [`Performer`] that owns the
/// grid + current SGR attributes.
pub struct Parser {
    inner: vte::Parser,
    performer: Performer,
}

impl Parser {
    /// Build a parser bound to `grid`, with no upstream reply channel — DSR /
    /// XTVERSION queries will be silently dropped.
    pub fn new(grid: Grid) -> Self {
        Self { inner: vte::Parser::new(), performer: Performer::new(grid, None) }
    }

    /// Construct a parser that can send replies (DSR, DA, XTVERSION, focus
    /// reporting) back to the pty via the given channel.
    pub fn new_with_reply(grid: Grid, reply_tx: Sender<Vec<u8>>) -> Self {
        Self { inner: vte::Parser::new(), performer: Performer::new(grid, Some(reply_tx)) }
    }

    /// Tell the parser the theme default foreground colour. Used to answer
    /// `OSC 10 ; ? ST` queries from the shell/TUI. nvim sends OSC 10/11
    /// at startup to learn the terminal's defaults so it can render cells
    /// declared with `fg=NONE`/`bg=NONE` consistently — see issue #369.
    pub fn set_theme_fg(&mut self, r: u8, g: u8, b: u8) {
        self.performer.theme_fg = Some((r, g, b));
    }

    /// Tell the parser the theme default background colour. Used to answer
    /// `OSC 11 ; ? ST` queries (see [`Parser::set_theme_fg`]).
    pub fn set_theme_bg(&mut self, r: u8, g: u8, b: u8) {
        self.performer.theme_bg = Some((r, g, b));
    }

    /// Tell the parser the theme cursor colour. Used to answer
    /// `OSC 12 ; ? ST` queries. When unset, OSC 12 falls back to the
    /// theme foreground.
    pub fn set_theme_cursor(&mut self, r: u8, g: u8, b: u8) {
        self.performer.theme_cursor = Some((r, g, b));
    }

    /// Whether DECSET ?1004 (focus reporting) is currently enabled. App should
    /// send `\e[I` / `\e[O` on focus in/out when this is true.
    pub fn focus_reporting_enabled(&self) -> bool {
        self.performer.focus_reporting
    }

    /// Feed raw bytes from the pty. Drains any queued events for the caller.
    ///
    /// Implements an ASCII SWAR fast-path: while the underlying vte state
    /// machine is in the Ground state (no escape sequence in flight), we
    /// scan the input via `memchr` for the next byte that vte would actually
    /// need to dispatch (ESC `0x1B`, BEL `0x07`, or anything outside the
    /// `[0x20, 0x7E]` printable-ASCII range), bulk-print the safe ASCII run
    /// straight into the grid, and only hand the remainder to vte. Hot
    /// payloads like `cat largefile` are ~99 % printable ASCII, so this
    /// bypasses vte's byte-at-a-time state machine for the common case while
    /// keeping behaviour identical to feeding the whole slice through vte.
    pub fn advance(&mut self, bytes: &[u8]) -> Vec<VtEvent> {
        let mut i = 0;
        let len = bytes.len();
        while i < len {
            if self.performer.ground {
                // memchr3 for ESC / BEL / LF — the three commonest break
                // bytes — gives us a cheap upper bound on the run length.
                // We then scalar-verify the prefix is entirely printable
                // [0x20, 0x7E]; the first non-printable byte ends the run.
                let upper = memchr::memchr3(0x1B, 0x07, 0x0A, &bytes[i..]).unwrap_or(len - i);
                let mut run_end = 0;
                while run_end < upper {
                    let b = bytes[i + run_end];
                    if !(0x20..=0x7E).contains(&b) {
                        break;
                    }
                    run_end += 1;
                }
                if run_end > 0 {
                    // SAFETY: every byte in [i..i+run_end] is in [0x20, 0x7E],
                    // i.e. valid 1-byte UTF-8 = the same code point as the byte.
                    for &b in &bytes[i..i + run_end] {
                        self.performer.grid.put_char_linked(
                            b as char,
                            self.performer.fg,
                            self.performer.bg,
                            self.performer.flags,
                            self.performer.current_hyperlink,
                        );
                    }
                    // REP (CSI b) needs the most-recent printable; the fast
                    // path bypasses Performer::print so update it here.
                    if let Some(&b) = bytes[i..i + run_end].last() {
                        self.performer.last_printed_char = Some(b as char);
                    }
                    i += run_end;
                    continue;
                }
                // First byte is non-printable — feed exactly that byte to
                // vte. vte will either dispatch it (still Ground after) or
                // start consuming an escape (ground flips false). The
                // Performer callbacks below update `self.performer.ground`.
                self.performer.ground = false;
                self.inner.advance(&mut self.performer, &bytes[i..i + 1]);
                // If vte stayed in Ground (execute() or print()), the
                // callback has already set ground=true. If not, leave it
                // false so the next iteration feeds bytes through vte until
                // a dispatch callback flips it back to Ground.
                i += 1;
            } else {
                // Escape in flight — feed bytes through vte one at a time
                // and let the dispatch callbacks decide when we're back in
                // Ground. Feeding the remainder en bloc would work too, but
                // we want to return to fast-path as soon as possible, so
                // stop the moment ground flips back to true.
                let start = i;
                while i < len && !self.performer.ground {
                    self.inner.advance(&mut self.performer, &bytes[i..i + 1]);
                    i += 1;
                }
                debug_assert!(i > start, "vte must consume at least one byte per iteration");
            }
        }
        std::mem::take(&mut self.performer.events)
    }

    /// Borrow the underlying [`Grid`] — used by the renderer to read cells.
    pub fn grid(&self) -> &Grid {
        &self.performer.grid
    }

    /// Mutably borrow the [`Grid`] — used by the host on resize, scrollback
    /// scroll, and selection clears.
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

    /// Latest OSC 7 working directory (sticky), or `None` if the shell hasn't
    /// reported one. Stored as a filesystem path (the `file://host/` prefix
    /// is stripped at parse time); used by the tab-title renderer to show
    /// `parent/leaf` of the current cwd.
    pub fn cwd(&self) -> Option<&str> {
        self.performer.cwd.as_deref()
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
    /// Latest OSC 7 working directory (sticky), filesystem path with the
    /// `file://host/` prefix already stripped. `None` until the shell sends
    /// one — modern zsh/bash/fish ship with cwd-reporting prompts.
    cwd: Option<String>,
    reply_tx: Option<Sender<Vec<u8>>>,
    /// Theme default foreground (sRGB), used to answer OSC 10 `?` queries.
    /// `None` means the parser was never told a theme — query replies are
    /// suppressed in that case so we don't lie to the shell.
    theme_fg: Option<(u8, u8, u8)>,
    /// Theme default background (sRGB), used to answer OSC 11 `?` queries.
    /// nvim queries this to colour cells painted with `bg=NONE` (e.g.
    /// neo-tree icon cells); without a reply nvim guesses (27,29,30)
    /// instead of Sonic's actual theme bg — see issue #369.
    theme_bg: Option<(u8, u8, u8)>,
    /// Theme cursor colour (sRGB), used to answer OSC 12 `?` queries.
    /// Falls back to `theme_fg` if unset.
    theme_cursor: Option<(u8, u8, u8)>,
    /// DECSTBM scrolling region top margin (visible-row, 0-based,
    /// inclusive). `None` means "no region set — full screen".
    scroll_top: Option<u16>,
    /// DECSTBM scrolling region bottom margin (visible-row, 0-based,
    /// inclusive).
    scroll_bottom: Option<u16>,
    /// Tracks whether the underlying vte state machine is in the Ground
    /// state (no escape sequence currently being consumed). Maintained
    /// externally: set to `true` after every dispatch callback fires
    /// (`print` / `execute` / `csi_dispatch` / `osc_dispatch` /
    /// `esc_dispatch` / `unhook`), set to `false` inside `Parser::advance`
    /// just before feeding the first byte of a potential escape, and held
    /// `false` while inside a DCS passthrough (`hook` … `unhook`).
    /// The ASCII fast-path in `Parser::advance` is only taken when this is
    /// `true`.
    ground: bool,
    /// Most-recently-printed graphic character, for CSI `b` (REP).
    /// ECMA-48: REP repeats the GRAPHIC CHARACTER immediately preceding
    /// REP in the data stream. Reset when a control function intervenes.
    last_printed_char: Option<char>,
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
            cwd: None,
            reply_tx,
            theme_fg: None,
            theme_bg: None,
            theme_cursor: None,
            scroll_top: None,
            scroll_bottom: None,
            ground: true,
            last_printed_char: None,
        }
    }

    /// Resolve the active scroll region, defaulting to the full
    /// visible grid when DECSTBM has not been set. Used by every
    /// scroll-emitting opcode (CSI S, CSI T, IND-at-bottom-margin,
    /// RI-at-top-margin).
    fn effective_scroll_region(&self) -> (u16, u16) {
        let rows = self.grid.rows;
        let top = self.scroll_top.unwrap_or(0);
        let bot = self.scroll_bottom.unwrap_or(rows.saturating_sub(1));
        (top, bot)
    }

    fn reply(&self, bytes: &[u8]) {
        if let Some(tx) = &self.reply_tx {
            let _ = tx.send(bytes.to_vec());
        }
    }

    fn reset_last_printed_char(&mut self) {
        self.last_printed_char = None;
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
        self.reset_last_printed_char();
        for slice in params.iter() {
            let code = slice.first().copied().unwrap_or(0);
            match code {
                25 => self.events.push(VtEvent::CursorVisibility(set)),
                47 => {
                    let before = self.grid.is_alt();
                    if set {
                        self.grid.enter_alt_screen();
                    } else {
                        self.grid.leave_alt_screen();
                    }
                    let (r, c) = (self.grid.cursor.row, self.grid.cursor.col);
                    let after = self.grid.is_alt();
                    let sr = if set { "h" } else { "l" };
                    tracing::debug!(
                        target: "sonic_vt::alt",
                        "private mode CSI ?47{sr}: alt_screen_active={before}→{after}, cursor=({r},{c})"
                    );
                }
                1047 => {
                    // Same as ?47 — alt-screen switch WITHOUT cursor save/restore.
                    // Distinct from ?1049 (which also saves/restores the cursor)
                    // and from ?1048 (cursor save/restore only).
                    let before = self.grid.is_alt();
                    if set {
                        self.grid.enter_alt_screen();
                    } else {
                        self.grid.leave_alt_screen();
                    }
                    let (r, c) = (self.grid.cursor.row, self.grid.cursor.col);
                    let after = self.grid.is_alt();
                    let sr = if set { "h" } else { "l" };
                    tracing::debug!(
                        target: "sonic_vt::alt",
                        "private mode CSI ?1047{sr}: alt_screen_active={before}→{after}, cursor=({r},{c})"
                    );
                }
                1048 => {
                    // Save / restore cursor only (DECSC / DECRC equivalent).
                    let before = self.grid.is_alt();
                    if set {
                        self.saved_cursor = Some(self.grid.cursor);
                    } else if let Some(c) = self.saved_cursor {
                        self.grid.goto(c.row, c.col);
                    }
                    let (r, c) = (self.grid.cursor.row, self.grid.cursor.col);
                    let sr = if set { "h" } else { "l" };
                    tracing::debug!(
                        target: "sonic_vt::alt",
                        "private mode CSI ?1048{sr}: alt_screen_active={before}→{before}, cursor=({r},{c})"
                    );
                }
                1049 => {
                    let before = self.grid.is_alt();
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
                    let (r, c) = (self.grid.cursor.row, self.grid.cursor.col);
                    let after = self.grid.is_alt();
                    let sr = if set { "h" } else { "l" };
                    tracing::debug!(
                        target: "sonic_vt::alt",
                        "private mode CSI ?1049{sr}: alt_screen_active={before}→{after}, cursor=({r},{c})"
                    );
                }
                2004 => self.bracketed_paste = set,
                1006 => self.mouse_sgr = set,
                1004 => self.focus_reporting = set,
                2026 => { /* synchronized output (BSU/ESU) — accept silently for now;
                     defer-paint optimisation tracked separately. Prevents future
                     smear classes from apps that wrap updates in ?2026 h/l. */
                }
                _ => {}
            }
        }
    }
}

/// Parse an OSC 7 payload (typically `file://host/path`) into a filesystem
/// path. Strips the scheme + host, and percent-decodes `%XX` escapes so
/// names with spaces / unicode round-trip correctly. Empty / malformed
/// inputs return an empty string.
pub fn parse_osc7_cwd(raw: &str) -> String {
    let stripped = raw.strip_prefix("file://").unwrap_or(raw);
    // After `file://` the next `/` starts the absolute path; anything
    // before it is the (often empty) hostname which we discard.
    let path_part = match stripped.find('/') {
        Some(i) => &stripped[i..],
        None => stripped,
    };
    percent_decode(path_part)
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex_nibble(bytes[i + 1]), hex_nibble(bytes[i + 2])) {
                out.push((h << 4) | l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| s.to_string())
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(10 + b - b'a'),
        b'A'..=b'F' => Some(10 + b - b'A'),
        _ => None,
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
        self.last_printed_char = Some(c);
        self.ground = true;
    }

    fn execute(&mut self, byte: u8) {
        self.reset_last_printed_char();
        match byte {
            0x07 => self.events.push(VtEvent::Bell),
            0x08 => self.grid.backspace(),
            0x09 => self.grid.tab(),
            0x0A..=0x0C => {
                // LF/VT/FF — like IND, must scroll the active region
                // (not the whole grid) when at the bottom margin so
                // DECSTBM works for shells/apps that use LF rather
                // than IND. #348.
                let (top, bot) = self.effective_scroll_region();
                if self.grid.cursor.row == bot
                    && (self.scroll_top.is_some() || self.scroll_bottom.is_some())
                {
                    self.grid.scroll_region_up(top, bot, 1);
                } else {
                    self.grid.linefeed();
                }
            }
            0x0D => self.grid.carriage_return(),
            _ => {}
        }
        // NB: do NOT set ground=true here. vte may call execute() while still
        // inside an ESC/CSI/OSC/DCS state machine (C0 bytes are dispatched
        // even mid-escape). Resuming the SWAR fast-path here would consume
        // the remainder of the escape sequence as printable text.
        self.ground = false;
    }

    fn csi_dispatch(&mut self, params: &Params, inter: &[u8], _ignore: bool, action: char) {
        self.ground = false;
        if action != 'b' {
            self.reset_last_printed_char();
        }
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
            'J' => {
                let mode = p0();
                let (r, c) = (self.grid.cursor.row, self.grid.cursor.col);
                let (rows, cols) = (self.grid.rows, self.grid.cols);
                let will_blank = match mode {
                    0 => format!(
                        "rows ({r},{c})..({r},{}) + ({},0)..({},{})",
                        cols.saturating_sub(1),
                        r + 1,
                        rows.saturating_sub(1),
                        cols.saturating_sub(1)
                    ),
                    1 => format!("(0,0)..({r},{c}) inclusive"),
                    2 | 3 => "entire screen".to_string(),
                    _ => "<unknown mode, no-op>".to_string(),
                };
                tracing::debug!(
                    target: "sonic_vt::erase",
                    "CSI {mode}J: cursor=({r},{c}), grid_size=({rows},{cols}), will_blank={will_blank}"
                );
                match mode {
                    0 => self.grid.erase_below(),
                    1 => self.grid.erase_above(),
                    2 | 3 => self.grid.erase_screen(),
                    _ => {}
                }
            }
            'K' => {
                let mode = p0();
                let (r, c) = (self.grid.cursor.row, self.grid.cursor.col);
                let (rows, cols) = (self.grid.rows, self.grid.cols);
                let will_blank = match mode {
                    0 => format!("cells ({r},{c})..({r},{})", cols.saturating_sub(1)),
                    1 => format!("cells ({r},0)..({r},{c}) inclusive"),
                    2 => format!("cells ({r},0)..({r},{})", cols.saturating_sub(1)),
                    _ => "<unknown mode, no-op>".to_string(),
                };
                tracing::debug!(
                    target: "sonic_vt::erase",
                    "CSI {mode}K: cursor=({r},{c}), grid_size=({rows},{cols}), will_blank={will_blank}"
                );
                match mode {
                    0 => self.grid.erase_line_to_end(),
                    1 => self.grid.erase_line_to_start(),
                    2 => self.grid.erase_line(),
                    _ => {}
                }
            }
            'L' => {
                // CSI Ps L — IL (Insert Line). Insert n blank lines at the
                // cursor row, pushing the rest of the scroll region down.
                // ECMA-48: no-op when cursor is outside the active region.
                // xterm behaviour: cursor moves to column 0.
                let n = p0().max(1);
                let (top, bot) = self.effective_scroll_region();
                let cur = self.grid.cursor.row;
                if cur >= top && cur <= bot {
                    self.grid.scroll_region_down(cur, bot, n);
                    self.grid.cursor.col = 0;
                }
            }
            'M' => {
                // CSI Ps M — DL (Delete Line). Delete n lines starting at
                // the cursor row, pulling the region below up. Cursor->col 0.
                let n = p0().max(1);
                let (top, bot) = self.effective_scroll_region();
                let cur = self.grid.cursor.row;
                if cur >= top && cur <= bot {
                    self.grid.scroll_region_up(cur, bot, n);
                    self.grid.cursor.col = 0;
                }
            }
            'm' => self.apply_sgr(params),
            '@' => {
                // ICH — Insert n blank cells at the cursor on the current
                // row, shifting trailing cells right and dropping overflow.
                let n = p0().max(1) as usize;
                let cur = self.grid.cursor;
                self.grid.insert_cells(cur.row, cur.col, n);
            }
            'P' => {
                // DCH — Delete n cells at the cursor, shifting trailing
                // cells left and filling the right edge with blanks.
                let n = p0().max(1) as usize;
                let cur = self.grid.cursor;
                self.grid.delete_cells(cur.row, cur.col, n);
            }
            'X' => {
                // ECH — Erase n cells starting at the cursor with the
                // default (blank) cell. Cursor is unchanged. neo-tree's
                // per-row tail-clear pattern depends on this (#359).
                let n = p0().max(1) as usize;
                let cur = self.grid.cursor;
                self.grid.erase_cells(cur.row, cur.col, n);
            }
            'G' | '`' => {
                // CHA (G) / HPA (`) — Cursor to column p0 (1-based) on the
                // current row.
                let col_1 = p0().max(1);
                let row = self.grid.cursor.row;
                self.grid.goto(row, col_1.saturating_sub(1));
            }
            'd' => {
                // VPA — Cursor to row p0 (1-based), column unchanged.
                let row_1 = p0().max(1);
                let col = self.grid.cursor.col;
                self.grid.goto(row_1.saturating_sub(1), col);
            }
            'b' => {
                // REP — Repeat last printable character n times at cursor.
                let n = p0().max(1) as usize;
                if let Some(ch) = self.last_printed_char {
                    for _ in 0..n {
                        self.grid.put_char_linked(
                            ch,
                            self.fg,
                            self.bg,
                            self.flags,
                            self.current_hyperlink,
                        );
                    }
                }
            }
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
            'S' => {
                // CSI Ps S — Scroll Up (SU). Scrolls the active region
                // up by `n` lines, fills bottom with blanks. Dest rows
                // are marked dirty by the grid, which is the fix for
                // #348 (stale LineQuadCache entries after region scroll).
                let n = p0().max(1);
                let (top, bot) = self.effective_scroll_region();
                self.grid.scroll_region_up(top, bot, n);
            }
            'T' => {
                // CSI Ps T — Scroll Down (SD).
                let n = p0().max(1);
                let (top, bot) = self.effective_scroll_region();
                self.grid.scroll_region_down(top, bot, n);
            }
            'r' => {
                // CSI Ps ; Ps r — DECSTBM Set Top and Bottom Margins.
                // Both omitted / 0 / out-of-range -> reset to full
                // screen. Cursor moves to home as per spec.
                let rows = self.grid.rows;
                let top_p = p0();
                let bot_p = p1();
                let cur_before = (self.grid.cursor.row, self.grid.cursor.col);
                let new_top = if top_p == 0 { 0 } else { top_p.saturating_sub(1) };
                let new_bot =
                    if bot_p == 0 { rows.saturating_sub(1) } else { bot_p.saturating_sub(1) };
                let (applied_top, applied_bot) = if new_top < new_bot && new_bot < rows {
                    self.scroll_top = Some(new_top);
                    self.scroll_bottom = Some(new_bot);
                    (new_top, new_bot)
                } else {
                    self.scroll_top = None;
                    self.scroll_bottom = None;
                    (0, rows.saturating_sub(1))
                };
                self.grid.goto(0, 0);
                tracing::debug!(
                    target: "sonic_vt::stbm",
                    "CSI {top_p};{bot_p}r DECSTBM: parsed=({new_top},{new_bot}), applied=({applied_top},{applied_bot}), grid_rows={rows}, cursor {:?}→(0,0)",
                    cur_before
                );
            }
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool) {
        self.ground = false;
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
            Some(7) => {
                // OSC 7 ; file://<host>/<path> ST — shell-reported cwd.
                // Used by the tab-title renderer to show `parent/leaf`.
                // We are permissive: accept the raw payload even when it
                // doesn't start with `file://` (some shells skip the
                // scheme), strip the host component when present, and
                // percent-decode the path so spaces/unicode survive.
                if let Some(raw) = params.get(1).and_then(|s| std::str::from_utf8(s).ok()) {
                    let path = parse_osc7_cwd(raw);
                    if !path.is_empty() {
                        self.cwd = Some(path);
                    }
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
                // doesn't match Sonic's actual theme bg — neo-tree
                // icon cells (painted with `bg=NONE`) then visibly
                // differ from the surrounding theme-clear surface.
                // See issue #369.
                //
                // OSC 10/11/12 *set* (payload is a colour, not `?`)
                // is intentionally not implemented yet — diagnosis
                // shows query-reply is sufficient to fix #369.
                let payload = params.get(1).and_then(|s| std::str::from_utf8(s).ok());
                if payload != Some("?") {
                    return;
                }
                let rgb = match code {
                    10 => self.theme_fg,
                    11 => self.theme_bg,
                    12 => self.theme_cursor.or(self.theme_fg),
                    _ => None,
                };
                let Some((r, g, b)) = rgb else { return };
                let terminator: &[u8] = if bell_terminated { b"\x07" } else { b"\x1b\\" };
                let mut buf = Vec::with_capacity(24);
                buf.extend_from_slice(b"\x1b]");
                buf.extend_from_slice(code.to_string().as_bytes());
                buf.extend_from_slice(
                    format!(";rgb:{r:02x}{r:02x}/{g:02x}{g:02x}/{b:02x}{b:02x}").as_bytes(),
                );
                buf.extend_from_slice(terminator);
                self.reply(&buf);
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
                //   B → command-line edit start / command start in Sonic
                //   C → command output start
                //   D [; exit_code] → command finished
                let kind = params.get(1).and_then(|s| s.first().copied());
                match kind {
                    Some(b'A') => {
                        self.grid.record_prompt_start();
                        self.events.push(VtEvent::Command(CommandEvent::PromptStart));
                    }
                    Some(b'B') | Some(b'C') => {
                        self.events.push(VtEvent::Command(CommandEvent::CmdStart));
                    }
                    Some(b'D') => {
                        let exit_i32 = params
                            .get(2)
                            .and_then(|s| std::str::from_utf8(s).ok())
                            .and_then(|s| s.parse::<i32>().ok());
                        self.grid.record_prompt_end(exit_i32);
                        let exit = exit_i32.and_then(|n| u8::try_from(n).ok());
                        self.events.push(VtEvent::Command(CommandEvent::CmdEnd(exit)));
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn hook(&mut self, _: &Params, _: &[u8], _: bool, _: char) {
        // Entering DCS passthrough — stay out of the fast-path until unhook.
        self.ground = false;
    }
    fn put(&mut self, _byte: u8) {
        self.ground = false;
    }
    fn unhook(&mut self) {
        self.ground = false;
    }
    fn esc_dispatch(&mut self, _intermediates: &[u8], _ignore: bool, byte: u8) {
        self.ground = false;
        self.reset_last_printed_char();
        match byte {
            b'D' => {
                // IND — Index. Move cursor down one line; if at the
                // bottom margin of the scroll region, scroll the
                // region up. Must respect DECSTBM (#348).
                let (top, bot) = self.effective_scroll_region();
                if self.grid.cursor.row == bot {
                    self.grid.scroll_region_up(top, bot, 1);
                } else {
                    let new_row = (self.grid.cursor.row + 1).min(self.grid.rows.saturating_sub(1));
                    let col = self.grid.cursor.col;
                    self.grid.goto(new_row, col);
                }
            }
            b'M' => {
                // RI — Reverse Index. Move cursor up; if at top
                // margin, scroll the region down.
                let (top, bot) = self.effective_scroll_region();
                if self.grid.cursor.row == top {
                    self.grid.scroll_region_down(top, bot, 1);
                } else {
                    let new_row = self.grid.cursor.row.saturating_sub(1);
                    let col = self.grid.cursor.col;
                    self.grid.goto(new_row, col);
                }
            }
            b'E' => {
                // NEL — Next Line. Like IND, but also moves cursor to col 0.
                let (top, bot) = self.effective_scroll_region();
                if self.grid.cursor.row == bot {
                    self.grid.scroll_region_up(top, bot, 1);
                    self.grid.goto(self.grid.cursor.row, 0);
                } else {
                    let new_row = (self.grid.cursor.row + 1).min(self.grid.rows.saturating_sub(1));
                    self.grid.goto(new_row, 0);
                }
            }
            _ => {}
        }
    }
}
