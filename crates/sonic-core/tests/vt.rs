//! Integration tests for `sonic_core::vt`.
//!
//! Migrated from inline `#[cfg(test)] mod tests` in src/vt.rs as part of
//! the per-crate tests/ folder restructure. Adds alt-screen / DEC-mode
//! tests introduced in v0.5.

use sonic_core::grid;
use sonic_core::grid::{CellFlags, Color, Grid};
use sonic_core::vt::VtEvent;
use sonic_core::vt::*;

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
    // ED 2 erases but does NOT move the cursor (per xterm).
}

#[test]
fn ed0_only_erases_below_cursor() {
    let mut p = Parser::new(Grid::new(5, 3));
    p.advance(b"aaa\r\nbbb\r\nccc");
    p.advance(b"\x1b[1;2H"); // row 1 col 2 (1-indexed)
    p.advance(b"\x1b[0J");
    assert_eq!(p.grid().row(0)[0].ch, 'a');
    assert_eq!(p.grid().row(0)[1].ch, ' ');
    assert_eq!(p.grid().row(1)[0].ch, ' ');
    assert_eq!(p.grid().row(2)[0].ch, ' ');
}

#[test]
fn ed1_erases_above_cursor() {
    let mut p = Parser::new(Grid::new(3, 3));
    p.advance(b"aaa\r\nbbb\r\nccc");
    p.advance(b"\x1b[2;2H");
    p.advance(b"\x1b[1J");
    assert_eq!(p.grid().row(0)[0].ch, ' ');
    assert_eq!(p.grid().row(1)[1].ch, ' ');
    assert_eq!(p.grid().row(2)[0].ch, 'c');
}

#[test]
fn el_modes_distinct() {
    let mut p = Parser::new(Grid::new(5, 2));
    p.advance(b"abcde\r\nfghij");
    p.advance(b"\x1b[1;3H");
    p.advance(b"\x1b[0K"); // erase to end
    assert_eq!(p.grid().row(0)[1].ch, 'b');
    assert_eq!(p.grid().row(0)[2].ch, ' ');
    p.advance(b"\x1b[2;3H");
    p.advance(b"\x1b[1K"); // erase to start
    assert_eq!(p.grid().row(1)[0].ch, ' ');
    assert_eq!(p.grid().row(1)[3].ch, 'i');
}

#[test]
fn shell_prompt_redraw_preserves_above_cursor() {
    // The real-world bug the e2e test caught: a shell that runs `ls`,
    // sees the output, then redraws its prompt via ED 0 should NOT
    // wipe prior output.
    let mut p = Parser::new(Grid::new(20, 4));
    p.advance(b"prompt$ ls\r\nfile1 file2\r\nprompt$ ");
    p.advance(b"\x1b[0J");
    assert_eq!(p.grid().row(0)[0].ch, 'p');
    assert_eq!(p.grid().row(1)[0].ch, 'f');
    assert_eq!(p.grid().row(2)[0].ch, 'p');
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
    assert_eq!(p.grid().cursor, grid::Pos { row: 2, col: 4 });
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
fn osc8_tags_cells_then_untags() {
    let mut p = Parser::new(Grid::new(10, 1));
    p.advance(b"\x1b]8;;https://example.com\x07abc\x1b]8;;\x07de");
    let row = p.grid().row(0);
    assert!(row[0].hyperlink.is_some());
    assert!(row[1].hyperlink.is_some());
    assert!(row[2].hyperlink.is_some());
    assert_eq!(row[0].hyperlink, row[2].hyperlink, "same link reuses id");
    assert!(row[3].hyperlink.is_none());
    assert!(row[4].hyperlink.is_none());
    assert!(p.current_hyperlink().is_none());
}

#[test]
fn osc8_explicit_id_preserved_in_registry() {
    let mut p = Parser::new(Grid::new(10, 1));
    p.advance(b"\x1b]8;id=foo;https://example.com\x07x\x1b]8;;\x07");
    let row = p.grid().row(0);
    let hid = row[0].hyperlink.expect("hyperlink set");
    let link = p.hyperlinks().lookup(hid).expect("present");
    assert_eq!(link.id.as_deref(), Some("id=foo"));
    assert_eq!(link.uri, "https://example.com");
}

#[test]
fn osc8_empty_uri_clears_current_hyperlink() {
    let mut p = Parser::new(Grid::new(10, 1));
    p.advance(b"\x1b]8;;https://example.com\x07");
    assert!(p.current_hyperlink().is_some());
    p.advance(b"\x1b]8;;\x07");
    assert!(p.current_hyperlink().is_none());
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
#[test]
fn dec_1049h_enters_alt_screen_empty() {
    let mut p = Parser::new(Grid::new(10, 2));
    p.advance(b"hello");
    p.advance(b"\x1b[?1049h");
    assert!(p.grid().is_alt());
    for c in p.grid().row(0) {
        assert_eq!(c.ch, ' ');
    }
}

#[test]
fn dec_1049l_restores_primary_and_cursor() {
    let mut p = Parser::new(Grid::new(10, 2));
    p.advance(b"hello");
    let saved = p.grid().cursor;
    p.advance(b"\x1b[?1049h");
    p.advance(b"ALT");
    p.advance(b"\x1b[?1049l");
    assert!(!p.grid().is_alt());
    assert_eq!(p.grid().row(0)[0].ch, 'h');
    assert_eq!(p.grid().cursor, saved);
}

#[test]
fn dec_47_vs_1049_cursor_save_semantics() {
    // ?1049 explicitly stashes the pre-alt cursor and restores it on leave,
    // independent of any cursor moves the app made on the alt screen.
    let mut p = Parser::new(Grid::new(10, 2));
    p.advance(b"hello");
    let pre = p.grid().cursor;
    p.advance(b"\x1b[?1049h");
    // Move around on the alt screen, then leave.
    p.advance(b"\x1b[5;5H");
    p.advance(b"\x1b[?1049l");
    assert_eq!(p.grid().cursor, pre, "?1049l restores explicit pre-alt cursor");

    // ?47 has no explicit DEC saved_cursor side-channel (DECSC/DECRC do).
    // It must NOT seed the performer's saved_cursor — i.e., a later
    // ?1049l should be a no-op for cursor when no ?1049h preceded it.
    let mut p2 = Parser::new(Grid::new(10, 2));
    p2.advance(b"hi");
    p2.advance(b"\x1b[?47h");
    p2.advance(b"\x1b[?47l");
    // Subsequent stray ?1049l with no saved cursor must not panic / move.
    let before = p2.grid().cursor;
    p2.advance(b"\x1b[?1049l");
    assert_eq!(p2.grid().cursor, before);
}

#[test]
fn dec_1049h_repeated_does_not_clobber_saved_cursor() {
    // Real-world cause: vim / fzf preview pane re-enters alt screen
    // while already in alt. The second ?1049h must NOT save the alt-
    // screen cursor over the original primary cursor — leaving alt
    // afterwards must still land back at the original primary cursor.
    let mut p = Parser::new(Grid::new(10, 3));
    p.advance(b"abc\r\ndef");
    // cursor now somewhere on row 1
    let primary_cursor = p.grid().cursor;
    p.advance(b"\x1b[?1049h"); // enter alt
                               // move cursor inside the alt screen
    p.advance(b"\x1b[5;1H");
    // a stray re-entry that previously clobbered saved_cursor
    p.advance(b"\x1b[?1049h");
    // move again
    p.advance(b"\x1b[8;5H");
    p.advance(b"\x1b[?1049l"); // leave alt
    assert_eq!(p.grid().cursor, primary_cursor);
}

#[test]
fn dec_25_emits_cursor_visibility() {
    let mut p = Parser::new(Grid::new(5, 1));
    let evs = p.advance(b"\x1b[?25l");
    assert!(matches!(evs.last(), Some(VtEvent::CursorVisibility(false))));
    let evs = p.advance(b"\x1b[?25h");
    assert!(matches!(evs.last(), Some(VtEvent::CursorVisibility(true))));
}

#[test]
fn dec_2004_toggles_bracketed_paste() {
    let mut p = Parser::new(Grid::new(5, 1));
    assert!(!p.bracketed_paste_enabled());
    p.advance(b"\x1b[?2004h");
    assert!(p.bracketed_paste_enabled());
    p.advance(b"\x1b[?2004l");
    assert!(!p.bracketed_paste_enabled());
}

#[test]
fn dec_1006_toggles_mouse_sgr() {
    let mut p = Parser::new(Grid::new(5, 1));
    assert!(!p.mouse_sgr_enabled());
    p.advance(b"\x1b[?1006h");
    assert!(p.mouse_sgr_enabled());
    p.advance(b"\x1b[?1006l");
    assert!(!p.mouse_sgr_enabled());
}

#[test]
fn unknown_dec_modes_are_ignored() {
    let mut p = Parser::new(Grid::new(5, 1));
    let evs = p.advance(b"\x1b[?9999h\x1b[?12345lX");
    assert!(!evs.iter().any(|e| matches!(e, VtEvent::CursorVisibility(_))));
    assert_eq!(p.grid().row(0)[0].ch, 'X');
}

#[test]
fn csi_cursor_motion_bumps_revision_and_marks_dirty() {
    // Regression for B2 review: CSI A/B/C/D used to write self.grid.cursor
    // directly, bypassing the dirty/revision tracking. The cursor row would
    // not be marked dirty, so the renderer's per-row cache returned the
    // stale row (cursor trail / stuck cursor). Now they go through goto().
    let mut p = Parser::new(Grid::new(10, 5));
    p.advance(b"\x1b[3;3H"); // park cursor mid-screen first
    let r0 = p.grid().revision();
    // Render-side would have called clear_dirty() between frames.
    p.grid_mut().clear_dirty();
    p.advance(b"\x1b[A"); // cursor up
    assert!(p.grid().revision() > r0, "CSI A must bump revision");
    // Old row (3-indexed-from-1 = grid row 2) AND new row (1) should be dirty.
    assert!(p.grid().is_row_dirty(1), "new cursor row must be dirty");
    assert!(p.grid().is_row_dirty(2), "old cursor row must be dirty");

    p.grid_mut().clear_dirty();
    let r1 = p.grid().revision();
    p.advance(b"\x1b[2C"); // cursor right 2 (same row, dirty just current)
    assert!(p.grid().revision() > r1);
    assert!(p.grid().is_row_dirty(1));
}

#[test]
fn osc_133_a_records_prompt_start() {
    let mut p = Parser::new(Grid::new(10, 5));
    assert_eq!(p.grid().prompts_len(), 0);
    p.advance(b"\x1b]133;A\x07");
    assert_eq!(p.grid().prompts_len(), 1);
    let pr = p.grid().prompts().next().unwrap();
    assert_eq!(pr.start_row, 0);
    assert!(pr.end_row.is_none());
    assert!(pr.exit_code.is_none());
}

#[test]
fn osc_133_d_records_exit_code_on_last_prompt() {
    let mut p = Parser::new(Grid::new(10, 5));
    p.advance(b"\x1b]133;A\x07$ run\n");
    p.advance(b"\x1b]133;D;42\x07");
    let pr = p.grid().prompts().last().unwrap();
    assert_eq!(pr.exit_code, Some(42));
    assert!(pr.end_row.is_some());
}

#[test]
fn osc_133_d_without_exit_code_is_none() {
    let mut p = Parser::new(Grid::new(10, 5));
    p.advance(b"\x1b]133;A\x07");
    p.advance(b"\x1b]133;D\x07");
    let pr = p.grid().prompts().last().unwrap();
    assert_eq!(pr.exit_code, None);
    assert!(pr.end_row.is_some());
}

#[test]
fn osc_133_a_coalesces_on_same_row() {
    let mut p = Parser::new(Grid::new(10, 5));
    p.advance(b"\x1b]133;A\x07\x1b]133;A\x07\x1b]133;A\x07");
    assert_eq!(p.grid().prompts_len(), 1);
}

#[test]
fn osc_133_b_and_c_are_accepted_without_panic() {
    // B / C are no-ops in the current implementation but must not break
    // parsing of surrounding sequences.
    let mut p = Parser::new(Grid::new(10, 5));
    p.advance(b"\x1b]133;A\x07\x1b]133;B\x07X\x1b]133;C\x07Y");
    assert_eq!(p.grid().row(0)[0].ch, 'X');
    assert_eq!(p.grid().row(0)[1].ch, 'Y');
    assert_eq!(p.grid().prompts_len(), 1);
}

#[test]
fn osc_133_unknown_kind_is_ignored() {
    let mut p = Parser::new(Grid::new(10, 5));
    p.advance(b"\x1b]133;Z\x07X");
    assert_eq!(p.grid().prompts_len(), 0);
    assert_eq!(p.grid().row(0)[0].ch, 'X');
}
