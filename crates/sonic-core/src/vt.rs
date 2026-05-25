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

use vte::{Params, Perform};

use crate::grid::{CellFlags, Color, Grid};

/// Event surfaced to the host so it can update window chrome, clipboard, etc.
#[derive(Debug, Clone)]
pub enum VtEvent {
    SetTitle(String),
    Bell,
    Hyperlink { id: Option<String>, uri: String },
    Clipboard { selection: char, data: String },
}

/// Streaming parser wrapping `vte::Parser` and a [`Performer`] that owns the
/// grid + current SGR attributes.
pub struct Parser {
    inner: vte::Parser,
    performer: Performer,
}

impl Parser {
    pub fn new(grid: Grid) -> Self {
        Self { inner: vte::Parser::new(), performer: Performer::new(grid) }
    }

    /// Feed raw bytes from the pty. Drains any queued events for the caller.
    pub fn advance(&mut self, bytes: &[u8]) -> Vec<VtEvent> {
        for &b in bytes {
            self.inner.advance(&mut self.performer, b);
        }
        std::mem::take(&mut self.performer.events)
    }

    pub fn grid(&self) -> &Grid {
        &self.performer.grid
    }

    pub fn grid_mut(&mut self) -> &mut Grid {
        &mut self.performer.grid
    }
}

struct Performer {
    grid: Grid,
    fg: Color,
    bg: Color,
    flags: CellFlags,
    events: Vec<VtEvent>,
}

impl Performer {
    fn new(grid: Grid) -> Self {
        Self {
            grid,
            fg: Color::Default,
            bg: Color::Default,
            flags: CellFlags::empty(),
            events: Vec::new(),
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
        self.grid.put_char(c, self.fg, self.bg, self.flags);
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

    fn csi_dispatch(&mut self, params: &Params, _inter: &[u8], _ignore: bool, action: char) {
        let p0 = || params.iter().next().and_then(|s| s.first().copied()).unwrap_or(0);
        let p1 = || params.iter().nth(1).and_then(|s| s.first().copied()).unwrap_or(0);
        match action {
            'A' => {
                let n = p0().max(1);
                self.grid.cursor.row = self.grid.cursor.row.saturating_sub(n);
            }
            'B' => {
                let n = p0().max(1);
                self.grid.cursor.row =
                    (self.grid.cursor.row + n).min(self.grid.rows.saturating_sub(1));
            }
            'C' => {
                let n = p0().max(1);
                self.grid.cursor.col =
                    (self.grid.cursor.col + n).min(self.grid.cols.saturating_sub(1));
            }
            'D' => {
                let n = p0().max(1);
                self.grid.cursor.col = self.grid.cursor.col.saturating_sub(n);
            }
            'H' | 'f' => {
                let row = p0().saturating_sub(1);
                let col = p1().saturating_sub(1);
                self.grid.goto(row, col);
            }
            'J' => {
                self.grid.erase_screen();
                if p0() == 2 {
                    self.grid.goto(0, 0);
                }
            }
            'K' => self.grid.erase_line_to_end(),
            'm' => self.apply_sgr(params),
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
                    self.events.push(VtEvent::SetTitle(text.to_string()));
                }
            }
            Some(8) => {
                // OSC 8;params;uri ST — minimal hyperlink support
                let id = params.get(1).and_then(|s| std::str::from_utf8(s).ok());
                let uri = params.get(2).and_then(|s| std::str::from_utf8(s).ok());
                if let Some(uri) = uri {
                    self.events.push(VtEvent::Hyperlink {
                        id: id.filter(|s| !s.is_empty()).map(String::from),
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
            _ => {}
        }
    }

    fn hook(&mut self, _: &Params, _: &[u8], _: bool, _: char) {}
    fn put(&mut self, _byte: u8) {}
    fn unhook(&mut self) {}
    fn esc_dispatch(&mut self, _intermediates: &[u8], _ignore: bool, _byte: u8) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::Grid;

    fn parse(input: &[u8]) -> Parser {
        let mut p = Parser::new(Grid::new(20, 5));
        p.advance(input);
        p
    }

    #[test]
    fn prints_plain_text() {
        let p = parse(b"hello");
        assert_eq!(p.grid().row(0)[0].ch, 'h');
        assert_eq!(p.grid().row(0)[4].ch, 'o');
        assert_eq!(p.grid().cursor.col, 5);
    }

    #[test]
    fn sgr_red_then_reset() {
        let mut p = Parser::new(Grid::new(20, 1));
        p.advance(b"\x1b[31mR\x1b[0mN");
        assert_eq!(p.grid().row(0)[0].fg, Color::Indexed(1));
        assert_eq!(p.grid().row(0)[1].fg, Color::Default);
    }

    #[test]
    fn truecolor_fg() {
        let mut p = Parser::new(Grid::new(5, 1));
        p.advance(b"\x1b[38;2;10;20;30mX");
        assert_eq!(p.grid().row(0)[0].fg, Color::Rgb(10, 20, 30));
    }

    #[test]
    fn cup_moves_cursor_one_indexed() {
        let mut p = Parser::new(Grid::new(20, 5));
        p.advance(b"\x1b[3;7HZ");
        assert_eq!(p.grid().row(2)[6].ch, 'Z');
    }

    #[test]
    fn ed2_clears_screen() {
        let mut p = Parser::new(Grid::new(5, 2));
        p.advance(b"abc\x1b[2J");
        assert_eq!(p.grid().row(0)[0].ch, ' ');
        assert_eq!(p.grid().cursor, crate::grid::Pos::default());
    }

    #[test]
    fn osc_title_emits_event() {
        let mut p = Parser::new(Grid::new(5, 1));
        let evs = p.advance(b"\x1b]0;My Title\x07");
        assert!(matches!(evs.first(), Some(VtEvent::SetTitle(t)) if t == "My Title"));
    }

    #[test]
    fn cursor_motion_clamps() {
        let mut p = Parser::new(Grid::new(5, 3));
        p.advance(b"\x1b[100;100H");
        // CUP clamps to (rows-1, cols-1)
        assert_eq!(p.grid().cursor, crate::grid::Pos { row: 2, col: 4 });
    }

    #[test]
    fn cuu_cud_cuf_cub() {
        let mut p = Parser::new(Grid::new(10, 5));
        p.advance(b"\x1b[3;3H");
        p.advance(b"\x1b[2A"); // up 2
        assert_eq!(p.grid().cursor.row, 0);
        p.advance(b"\x1b[3B"); // down 3
        assert_eq!(p.grid().cursor.row, 3);
        p.advance(b"\x1b[4C"); // right 4
        assert_eq!(p.grid().cursor.col, 6);
        p.advance(b"\x1b[5D"); // left 5
        assert_eq!(p.grid().cursor.col, 1);
    }

    #[test]
    fn sgr_bold_italic_underline_compose() {
        let mut p = Parser::new(Grid::new(5, 1));
        p.advance(b"\x1b[1;3;4mX");
        let cell = &p.grid().row(0)[0];
        assert!(cell.flags.contains(CellFlags::BOLD));
        assert!(cell.flags.contains(CellFlags::ITALIC));
        assert!(cell.flags.contains(CellFlags::UNDERLINE));
    }

    #[test]
    fn sgr_bright_fg() {
        let mut p = Parser::new(Grid::new(5, 1));
        p.advance(b"\x1b[93mY"); // bright yellow
        assert_eq!(p.grid().row(0)[0].fg, Color::Indexed(11));
    }

    #[test]
    fn sgr_256_color_bg() {
        let mut p = Parser::new(Grid::new(5, 1));
        p.advance(b"\x1b[48;5;42mZ");
        assert_eq!(p.grid().row(0)[0].bg, Color::Indexed(42));
    }

    #[test]
    fn osc8_hyperlink_event() {
        let mut p = Parser::new(Grid::new(5, 1));
        let evs = p.advance(b"\x1b]8;;https://example.com\x07link\x1b]8;;\x07");
        assert!(evs
            .iter()
            .any(|e| matches!(e, VtEvent::Hyperlink { uri, .. } if uri == "https://example.com")));
    }

    #[test]
    fn bell_emits_event() {
        let mut p = Parser::new(Grid::new(5, 1));
        let evs = p.advance(b"\x07");
        assert!(matches!(evs.first(), Some(VtEvent::Bell)));
    }

    #[test]
    fn cr_lf_resets_column_and_advances_row() {
        let mut p = Parser::new(Grid::new(5, 3));
        p.advance(b"ab\r\ncd");
        assert_eq!(p.grid().row(0)[0].ch, 'a');
        assert_eq!(p.grid().row(1)[0].ch, 'c');
    }

    #[test]
    fn malformed_csi_does_not_panic() {
        let mut p = Parser::new(Grid::new(5, 2));
        // Junk sequences should be tolerated.
        p.advance(b"\x1b[\x1b[;;;m\x1b[?25hX");
        assert_eq!(p.grid().row(0)[0].ch, 'X');
    }

    #[test]
    fn utf8_multibyte_decoded() {
        let mut p = Parser::new(Grid::new(10, 1));
        p.advance("héllo→".as_bytes());
        assert_eq!(p.grid().row(0)[0].ch, 'h');
        assert_eq!(p.grid().row(0)[1].ch, 'é');
        assert_eq!(p.grid().row(0)[5].ch, '→');
    }
}
