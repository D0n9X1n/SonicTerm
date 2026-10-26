use super::*;
use crate::vt::vt_test_support::*;

/// Erasure retains colors but cannot turn cleared cells into visible underline runs.
#[test]
fn erase_fill_drops_underline_without_resetting_printed_rendition() {
    for erase in ["\x1b[0K", "\x1b[2J", "\x1b[2X", "\x1b[2@", "\x1b[2P"] {
        let mut parser = Parser::new(Grid::new(8, 3));
        parser.advance(b"\x1b[31;44;4:3;58:5:2m");
        parser.advance(erase.as_bytes());
        for cell in parser.grid().row(0).iter() {
            assert!(!cell.flags.contains(CellFlags::UNDERLINE), "{erase:?}");
        }
        let fill_col = if erase == "\x1b[2P" { 7 } else { 0 };
        assert_eq!(parser.grid().row(0)[fill_col].bg, Color::Indexed(4), "{erase:?}");
        parser.advance(b"A B");
        let space = &parser.grid().row(0)[1];
        assert!(space.flags.contains(CellFlags::UNDERLINE));
        assert_eq!(space.underline_style(), UnderlineStyle::Curly);
        assert_eq!(space.underline_color(), Some(Color::Indexed(2)));
    }
    let mut parser = Parser::new(Grid::new(8, 2));
    parser.advance(b"\x1b[44;4m\x1b[2;1H\n");
    assert!(parser.grid().row(1).iter().all(|cell| !cell.flags.contains(CellFlags::UNDERLINE)));
    assert_eq!(parser.grid().row(1)[0].bg, Color::Indexed(4));
}

#[test]
fn ed3_removes_history_without_erasing_live_rows_or_cursor() {
    // Saved-history erasure must preserve live content, cursor state, and future history capacity.
    let mut parser = Parser::new(Grid::new(4, 2));
    parser.grid_mut().set_scrollback_limit(17);
    parser.advance(b"old\r\nkeep\r\nlive");
    assert_eq!(parser.grid().scrollback_len(), 1);
    let cursor = parser.grid().cursor;
    let sequence = parser.grid().content_seq();
    let revision = parser.grid().revision();
    let evicted = parser.grid().scrollback_evicted();
    parser.grid_mut().clear_dirty();

    parser.advance(b"\x1b[3J");

    assert_eq!(parser.grid().scrollback_len(), 0);
    assert_eq!(row_text(&parser, 0), "keep");
    assert_eq!(row_text(&parser, 1), "live");
    assert_eq!(parser.grid().cursor, cursor);
    assert_eq!(parser.grid().content_seq(), sequence);
    assert_eq!(parser.grid().visible_rows_changed_since(sequence).count(), 0);
    assert_ne!(parser.grid().revision(), revision);
    assert_eq!(parser.grid().dirty_count(), 2);
    assert_eq!(parser.grid().scrollback_evicted(), evicted + 1);
    parser.advance(b"\r\nnext");
    assert_eq!(parser.grid().scrollback_len(), 1);
}

#[test]
fn ed3_on_empty_primary_or_alternate_is_a_true_noop() {
    // Alternate ED3 cannot erase either its live cells or the hidden primary history.
    let mut parser = Parser::new(Grid::new(4, 2));
    parser.advance(b"live");
    parser.grid_mut().clear_dirty();
    let revision = parser.grid().revision();
    parser.advance(b"\x1b[3J");
    assert_eq!(row_text(&parser, 0), "live");
    assert_eq!(parser.grid().revision(), revision);
    assert_eq!(parser.grid().dirty_count(), 0);
    parser.advance(b"\r\nkeep\r\ntail");
    let history = parser.grid().scrollback_len();
    parser.advance(b"\x1b[?1049hALT");
    parser.grid_mut().clear_dirty();
    let alternate_revision = parser.grid().revision();
    parser.advance(b"\x1b[3J");
    assert_eq!(row_text(&parser, 0), "ALT ");
    assert_eq!(parser.grid().revision(), alternate_revision);
    assert_eq!(parser.grid().dirty_count(), 0);
    parser.advance(b"\x1b[?1049l");
    assert_eq!(parser.grid().scrollback_len(), history);
    assert_eq!(row_text(&parser, 0), "keep");
    assert_eq!(row_text(&parser, 1), "tail");
}

#[test]
fn ed3_preserves_rendition_margins_and_history_limit() {
    // Erasing history is not a terminal reset, a visible erase, or a temporary history-limit change.
    let mut parser = Parser::new(Grid::new(4, 3));
    parser.grid_mut().set_scrollback_limit(7);
    parser.advance(b"old\r\nkeep\r\nlive\r\ntail\x1b[2;3r\x1b[31;44m\x1b[3;2H");
    assert_eq!(parser.grid().scrollback_len(), 1);
    parser.advance(b"\x1b[3JX\n");
    assert_eq!(parser.grid().scrollback_len(), 0);
    assert_eq!(row_text(&parser, 0), "keep");
    assert_eq!(row_text(&parser, 1), "tXil");
    assert_eq!(parser.grid().row(1)[1].fg, Color::Indexed(1));
    assert_eq!(parser.grid().row(1)[1].bg, Color::Indexed(4));
    assert_eq!(parser.grid().row(2)[0].bg, Color::Indexed(4));
    parser.advance(b"\x1b[r\x1b[3;1H");
    for _ in 0..9 {
        parser.advance(b"\r\nnext");
    }
    assert_eq!(parser.grid().scrollback_len(), 7);
}

#[test]
fn visible_erase_modes_preserve_saved_history() {
    // ED0/1/2 retain their inclusive visible ranges and BCE without consuming history as ED3 does.
    for (mode, rows) in [
        (0, ["aaaa", "b   ", "    "]),
        (1, ["    ", "  bb", "cccc"]),
        (2, ["    ", "    ", "    "]),
    ] {
        let mut parser = Parser::new(Grid::new(4, 3));
        parser.advance(b"old\r\naaaa\r\nbbbb\r\ncccc\x1b[2;2H\x1b[44m");
        let cursor = parser.grid().cursor;
        let evicted = parser.grid().scrollback_evicted();
        parser.advance(format!("\x1b[{mode}J").as_bytes());
        for (row, expected) in rows.iter().enumerate() {
            assert_eq!(row_text(&parser, row as u16), *expected);
        }
        assert_eq!(parser.grid().row(1)[1].bg, Color::Indexed(4));
        assert_eq!(parser.grid().scrollback_len(), 1);
        assert_eq!(parser.grid().scrollback_row(0).unwrap()[0].ch, 'o');
        assert_eq!(parser.grid().scrollback_evicted(), evicted);
        assert_eq!(parser.grid().cursor, cursor);
    }
}

#[test]
fn dsr_reports_physical_column_without_consuming_delayed_wrap() {
    // Querying an insertion sentinel is observational; a following graphic still wraps.
    for (width, text) in [(4, "abc"), (4, "abcd"), (4, "ab中"), (1, "x")] {
        let (tx, rx) = crossbeam_channel::bounded(4);
        let mut parser = Parser::new_with_reply(Grid::new(width, 2), tx);
        parser.advance(text.as_bytes());
        let pending = parser.grid().pending_wrap();
        let cursor = parser.grid().cursor;
        for _ in 0..2 {
            parser.advance(b"\x1b[6n");
            assert_eq!(rx.try_recv().unwrap(), format!("\x1b[1;{width}R").into_bytes());
            assert_eq!(parser.grid().cursor, cursor);
            assert_eq!(parser.grid().pending_wrap(), pending);
        }
        parser.advance(b"Z");
        assert_eq!(parser.grid().cursor.row, u16::from(pending));
        if pending {
            assert_eq!(parser.grid().row(1)[0].ch, 'Z');
        }
    }
}

#[test]
fn dsr_observes_autowrap_off_and_normal_cursor_motion() {
    // Physical position replies remain bounded after clipped output and ordinary cursor movement.
    for width in [1, 4] {
        let (tx, rx) = crossbeam_channel::bounded(4);
        let mut parser = Parser::new_with_reply(Grid::new(width, 2), tx);
        parser.advance(b"\x1b[?7labcdef\x1b[6n");
        assert_eq!(rx.try_recv().unwrap(), format!("\x1b[1;{width}R").into_bytes());
        assert!(!parser.grid().pending_wrap());
        parser.advance(b"\x1b[2;1H\x1b[6n");
        assert_eq!(rx.try_recv().unwrap(), b"\x1b[2;1R");
        parser.advance(b"\x1b[999G\x1b[6n");
        assert_eq!(rx.try_recv().unwrap(), format!("\x1b[2;{width}R").into_bytes());
    }
}

#[test]
fn shell_redraw_of_a_wrapped_line_keeps_its_soft_wrap() {
    // zsh wraps by writing the next character, repaints the continuation row with CR + EL0,
    // then recolors the command word on the row above; the line must stay one logical line.
    let mut parser = Parser::new(Grid::new(10, 3));
    parser.advance(b"echo ~/.cla");
    assert!(parser.grid().row(1).soft_wrapped_from_previous());
    parser.advance(b"\r\x1b[Kaude.json");
    parser.advance(b"\x1b[A\x1b[9D\x1b[34mecho\x1b[39m\x1b[1B");
    assert!(parser.grid().row(1).soft_wrapped_from_previous());

    // Rewriting the wrapping row's last column is what ends the continuation.
    parser.advance(b"\x1b[1;10Hx");
    assert!(!parser.grid().row(1).soft_wrapped_from_previous());
}

#[test]
fn hard_advances_cancel_pending_wrap_at_physical_columns() {
    // A hard advance consumes delayed wrap without making the following graphic advance a second row.
    for control in [b"\n".as_slice(), b"\x0b", b"\x0c", b"\x1bD", b"\x1bE"] {
        for start_row in [0, 1] {
            let mut parser = Parser::new(Grid::new(4, 2));
            parser.advance(format!("\x1b[{};1Habcd", start_row + 1).as_bytes());
            assert!(parser.grid().pending_wrap());
            parser.advance(control);
            let col = if control == b"\x1bE" { 0 } else { 3 };
            assert!(!parser.grid().pending_wrap());
            assert_eq!(parser.grid().cursor.col, col, "control={control:?} row={start_row}");
            assert!(!parser.grid().row(1).soft_wrapped_from_previous());
            parser.advance(b"Z");
            assert_eq!(parser.grid().scrollback_len(), start_row);
            assert_eq!(parser.grid().row(1)[col as usize].ch, 'Z');
        }
    }
}

#[test]
fn hard_advances_respect_restricted_margins_and_physical_bottom() {
    // All five controls scroll only at the region bottom and clamp below it without adding history.
    for control in [b"\n".as_slice(), b"\x0b", b"\x0c", b"\x1bD", b"\x1bE"] {
        for row in 0..5 {
            let mut parser = Parser::new(Grid::new(4, 5));
            parser.advance(b"\x1b[1;1HAaaa\x1b[2;1HBbbb\x1b[3;1HCccc\x1b[4;1HDddd\x1b[5;1HEeee");
            parser.advance(b"\x1b[2;4r");
            parser.advance(format!("\x1b[{};2H", row + 1).as_bytes());
            parser.advance(control);
            let expected_rows = if row == 3 {
                ["Aaaa", "Cccc", "Dddd", "    ", "Eeee"]
            } else {
                ["Aaaa", "Bbbb", "Cccc", "Dddd", "Eeee"]
            };
            for (index, expected) in expected_rows.iter().enumerate() {
                assert_eq!(
                    row_text(&parser, index as u16),
                    *expected,
                    "control={control:?} row={row}"
                );
            }
            assert_eq!(parser.grid().scrollback_len(), 0, "control={control:?} row={row}");
            assert_eq!(parser.grid().cursor.row, if row == 3 { 3 } else { (row + 1).min(4) });
            assert_eq!(parser.grid().cursor.col, if control == b"\x1bE" { 0 } else { 1 });
        }
    }
}

#[test]
fn hard_advance_fullscreen_fill_and_column_policies_stay_distinct() {
    // LF/VT/FF use BCE while IND/NEL retain default fill; only NEL performs carriage return.
    for control in [b"\n".as_slice(), b"\x0b", b"\x0c", b"\x1bD", b"\x1bE"] {
        let mut parser = Parser::new(Grid::new(4, 2));
        parser.advance(b"top\x1b[2;2H\x1b[41m");
        parser.advance(control);
        assert_eq!(parser.grid().scrollback_len(), 1);
        assert_eq!(parser.grid().cursor.row, 1);
        assert_eq!(parser.grid().cursor.col, if control == b"\x1bE" { 0 } else { 1 });
        let background = if control.len() == 1 { Color::Indexed(1) } else { Color::Default };
        assert_eq!(parser.grid().row(1)[0].bg, background);
    }
}

#[test]
fn ris_resets_and_clears_screen() {
    let mut parser = Parser::new(Grid::new(8, 3));
    parser.advance(b"old text\nmore");

    parser.advance(b"\x1bc");

    assert_eq!(parser.grid().cursor.row, 0);
    assert_eq!(parser.grid().cursor.col, 0);
    assert_eq!(row_text(&parser, 0), "        ");
    assert_eq!(row_text(&parser, 1), "        ");
    assert_eq!(row_text(&parser, 2), "        ");
}

#[test]
fn ris_leaves_alt_screen_on_primary_blank() {
    let mut parser = Parser::new(Grid::new(8, 3));
    parser.advance(b"primary");
    parser.advance(b"\x1b[?1049h");
    parser.advance(b"alt");

    parser.advance(b"\x1bc");

    assert!(!parser.grid().is_alt());
    assert_eq!(parser.grid().cursor.row, 0);
    assert_eq!(parser.grid().cursor.col, 0);
    assert_eq!(row_text(&parser, 0), "        ");
}

#[test]
fn csi_g_moves_to_absolute_column() {
    let mut parser = Parser::new(Grid::new(8, 2));

    parser.advance(b"\x1b[5GZ");

    assert_eq!(parser.grid().cursor.row, 0);
    assert_eq!(parser.grid().cursor.col, 5);
    assert_eq!(row_text(&parser, 0), "    Z   ");
}

#[test]
fn bs_space_after_wide_char_clears_both_cells() {
    let mut parser = Parser::new(Grid::new(8, 2));

    parser.advance("中".as_bytes());
    parser.advance(b"\x08 ");

    let row = parser.grid().row(0);
    assert_eq!(row[0].ch, ' ');
    assert!(!row[0].flags.contains(CellFlags::WIDE));
    assert_eq!(row[1].ch, ' ');
    assert!(!row[1].flags.contains(CellFlags::WIDE_CONT));
    assert_eq!(parser.grid().cursor.col, 2);
}

#[test]
fn dec_save_restore_survives_scroll_region_reset() {
    let mut parser = Parser::new(Grid::new(12, 4));
    parser.advance(b"\x1b[4;7H");

    parser.advance(b"\x1b7\x1b[r\x1b8");

    assert_eq!(parser.grid().cursor.row, 3);
    assert_eq!(parser.grid().cursor.col, 6);
}

#[test]
fn ich_insert_before_yields_full_line_in_one_pass() {
    // Model the ZLE insert-before pattern with ICH (CSI @): type "11", home
    // the cursor, open two blank cells, then print "0." into them.
    let mut parser = Parser::new(Grid::new(8, 1));
    parser.advance(b"11"); // "11      ", cursor col 2
    parser.advance(b"\x1b[G"); // CHA → col 0
    parser.advance(b"\x1b[2@"); // ICH 2 → "  11"
    parser.advance(b"0."); // print → "0.11", cursor col 2
                           // The trailing '1' must be present immediately — no extra keystroke.
    assert_eq!(row_text(&parser, 0), "0.11    ");
}

#[test]
fn ich_re_dirties_row_so_trailing_cell_repaints_same_frame() {
    // After a frame has been consumed (clear_dirty), an insert-before edit
    // must re-dirty the row so the shifted-in trailing cells repaint in the
    // SAME frame. If ICH failed to dirty, the shifted cells would only show
    // after a later mutation — exactly the reported bug.
    let mut parser = Parser::new(Grid::new(8, 1));
    parser.advance(b"0.1");
    parser.advance(b"\x1b[G"); // home (goto dirties the row)…
    parser.grid_mut().clear_dirty(); // …so clear AFTER positioning to isolate ICH
    assert_eq!(parser.grid().dirty_count(), 0, "precondition: clean frame");
    parser.advance(b"\x1b[2@"); // ICH 2 at col 0
    assert!(parser.grid().is_row_dirty(0), "ICH-edited row must be dirty");
    assert_eq!(row_text(&parser, 0), "  0.1   ");
}

// --- Same-frame dirty propagation ------------------------------------------
//
// Each test clears prior damage (`clear_dirty`, modelling a consumed frame),
// performs exactly ONE mutation, then asserts the exact set of rows marked
// dirty BEFORE the renderer would consume the frame. This pins the VT/grid
// data layer so a scroll/erase/insert marks every row it visually changed in
// the SAME pass — no row can go stale until "the next keystroke".

/// Row indices currently marked dirty, ascending.
fn dirty_rows_vec(parser: &Parser) -> Vec<usize> {
    parser.grid().dirty_rows().collect()
}

#[test]
fn alt_screen_entry_dirties_every_row_same_frame() {
    let mut parser = Parser::new(Grid::new(8, 4));
    parser.advance(b"hi");
    parser.grid_mut().clear_dirty();
    assert_eq!(parser.grid().dirty_count(), 0, "precondition: clean frame");

    parser.advance(b"\x1b[?1049h"); // enter alt screen

    assert!(parser.grid().is_alt());
    assert_eq!(dirty_rows_vec(&parser), vec![0, 1, 2, 3], "alt entry repaints the whole screen");
}

#[test]
fn alt_screen_exit_dirties_every_row_same_frame() {
    let mut parser = Parser::new(Grid::new(8, 4));
    parser.advance(b"\x1b[?1049h");
    parser.advance(b"alt");
    parser.grid_mut().clear_dirty();
    assert_eq!(parser.grid().dirty_count(), 0, "precondition: clean frame");

    parser.advance(b"\x1b[?1049l"); // leave alt screen

    assert!(!parser.grid().is_alt());
    assert_eq!(dirty_rows_vec(&parser), vec![0, 1, 2, 3], "alt exit repaints the restored screen");
}

#[test]
fn decstbm_linefeed_at_bottom_margin_dirties_only_the_region() {
    // DECSTBM rows 2..5 (0-based 1..4). LF at the bottom margin scrolls the
    // region up; rows outside [1,4] must stay clean.
    let mut parser = Parser::new(Grid::new(8, 6));
    parser.advance(b"\x1b[2;5r"); // set margins; cursor -> home
    parser.advance(b"\x1b[5;1H"); // cursor to row 5 (0-based 4) = bottom margin
    parser.grid_mut().clear_dirty();
    assert_eq!(parser.grid().dirty_count(), 0, "precondition: clean frame");

    parser.advance(b"\n"); // LF at bottom margin -> scroll region up

    assert_eq!(
        dirty_rows_vec(&parser),
        vec![1, 2, 3, 4],
        "LF at the bottom margin dirties only the DECSTBM region"
    );
}

/// IND and NEL below a DECSTBM region clamp at the screen edge without scrolling the full grid.
#[test]
fn index_controls_below_scroll_region_do_not_scroll_screen() {
    for control in [b"\x1bD".as_slice(), b"\x1bE"] {
        let mut parser = Parser::new(Grid::new(4, 4));
        parser.advance(b"1111\r\n2222\r\n3333\r\n4444");
        parser.advance(b"\x1b[2;3r\x1b[4;4H");
        let before = (0..4)
            .map(|row| parser.grid().row(row).iter().map(|cell| cell.ch).collect::<String>())
            .collect::<Vec<_>>();
        let history = parser.grid().scrollback_len();

        parser.advance(control);

        let after = (0..4)
            .map(|row| parser.grid().row(row).iter().map(|cell| cell.ch).collect::<String>())
            .collect::<Vec<_>>();
        assert_eq!(after, before, "control={control:?}");
        assert_eq!(parser.grid().scrollback_len(), history, "control={control:?}");
        assert_eq!(parser.grid().cursor.row, 3, "control={control:?}");
    }
}

#[test]
fn decstbm_reverse_index_at_top_margin_dirties_only_the_region() {
    // RI at the top margin scrolls the region down; rows outside stay clean.
    let mut parser = Parser::new(Grid::new(8, 6));
    parser.advance(b"\x1b[2;5r"); // margins rows 1..4; cursor -> home
    parser.advance(b"\x1b[2;1H"); // cursor to row 2 (0-based 1) = top margin
    parser.grid_mut().clear_dirty();
    assert_eq!(parser.grid().dirty_count(), 0, "precondition: clean frame");

    parser.advance(b"\x1bM"); // RI at top margin -> scroll region down

    assert_eq!(
        dirty_rows_vec(&parser),
        vec![1, 2, 3, 4],
        "RI at the top margin dirties only the DECSTBM region"
    );
}

#[test]
fn insert_line_dirties_cursor_row_through_bottom_margin() {
    let mut parser = Parser::new(Grid::new(8, 6));
    parser.advance(b"\x1b[3;1H"); // cursor to row 3 (0-based 2)
    parser.grid_mut().clear_dirty();
    assert_eq!(parser.grid().dirty_count(), 0, "precondition: clean frame");

    parser.advance(b"\x1b[2L"); // IL 2

    assert_eq!(
        dirty_rows_vec(&parser),
        vec![2, 3, 4, 5],
        "IL dirties the cursor row through the bottom of the region"
    );
}

#[test]
fn delete_line_dirties_cursor_row_through_bottom_margin() {
    let mut parser = Parser::new(Grid::new(8, 6));
    parser.advance(b"\x1b[3;1H"); // cursor to row 3 (0-based 2)
    parser.grid_mut().clear_dirty();
    assert_eq!(parser.grid().dirty_count(), 0, "precondition: clean frame");

    parser.advance(b"\x1b[2M"); // DL 2

    assert_eq!(
        dirty_rows_vec(&parser),
        vec![2, 3, 4, 5],
        "DL dirties the cursor row through the bottom of the region"
    );
}

#[test]
fn reverse_index_off_the_top_margin_dirties_source_and_destination() {
    // With no DECSTBM the top margin is row 0; RI below it just moves the
    // cursor up one row and must dirty both the row it left and the row it
    // entered (so the cursor quad tracks in the same frame).
    let mut parser = Parser::new(Grid::new(8, 6));
    parser.advance(b"\x1b[4;1H"); // cursor to row 4 (0-based 3)
    parser.grid_mut().clear_dirty();
    assert_eq!(parser.grid().dirty_count(), 0, "precondition: clean frame");

    parser.advance(b"\x1bM"); // RI -> move up to row 2

    assert_eq!(
        dirty_rows_vec(&parser),
        vec![2, 3],
        "RI off the top margin dirties both the source and destination rows"
    );
}

#[test]
fn scroll_up_full_region_dirties_every_row() {
    let mut parser = Parser::new(Grid::new(8, 4));
    parser.advance(b"x");
    parser.grid_mut().clear_dirty();
    assert_eq!(parser.grid().dirty_count(), 0, "precondition: clean frame");

    parser.advance(b"\x1b[2S"); // SU 2 over the full (default) region

    assert_eq!(dirty_rows_vec(&parser), vec![0, 1, 2, 3], "full-region SU dirties every row");
}

#[test]
fn scroll_down_full_region_dirties_every_row() {
    let mut parser = Parser::new(Grid::new(8, 4));
    parser.advance(b"x");
    parser.grid_mut().clear_dirty();
    assert_eq!(parser.grid().dirty_count(), 0, "precondition: clean frame");

    parser.advance(b"\x1b[2T"); // SD 2 over the full (default) region

    assert_eq!(dirty_rows_vec(&parser), vec![0, 1, 2, 3], "full-region SD dirties every row");
}

#[test]
fn erase_below_dirties_cursor_row_through_screen_bottom() {
    let mut parser = Parser::new(Grid::new(8, 4));
    parser.advance(b"\x1b[2;1H"); // cursor to row 2 (0-based 1)
    parser.grid_mut().clear_dirty();
    assert_eq!(parser.grid().dirty_count(), 0, "precondition: clean frame");

    parser.advance(b"\x1b[0J"); // ED 0 (erase below)

    assert_eq!(
        dirty_rows_vec(&parser),
        vec![1, 2, 3],
        "ED-below dirties the cursor row through the screen bottom"
    );
}

#[test]
fn erase_screen_dirties_every_row() {
    let mut parser = Parser::new(Grid::new(8, 4));
    parser.advance(b"\x1b[2;1H");
    parser.grid_mut().clear_dirty();
    assert_eq!(parser.grid().dirty_count(), 0, "precondition: clean frame");

    parser.advance(b"\x1b[2J"); // ED 2 (erase whole screen)

    assert_eq!(dirty_rows_vec(&parser), vec![0, 1, 2, 3], "ED-all dirties every row");
}

#[test]
fn erase_line_dirties_only_the_cursor_row() {
    let mut parser = Parser::new(Grid::new(8, 4));
    parser.advance(b"\x1b[2;1H"); // cursor to row 1
    parser.grid_mut().clear_dirty();
    assert_eq!(parser.grid().dirty_count(), 0, "precondition: clean frame");

    parser.advance(b"\x1b[2K"); // EL 2 (erase whole line)

    assert_eq!(dirty_rows_vec(&parser), vec![1], "EL dirties only the cursor row");
}

#[test]
fn wide_char_edit_dirties_only_its_row_same_frame() {
    let mut parser = Parser::new(Grid::new(8, 3));
    parser.advance(b"\x1b[2;1H"); // cursor to row 1
    parser.grid_mut().clear_dirty();
    assert_eq!(parser.grid().dirty_count(), 0, "precondition: clean frame");

    parser.advance("中".as_bytes()); // wide glyph occupies cols 0-1 of row 1

    assert_eq!(dirty_rows_vec(&parser), vec![1], "a wide-cell edit dirties only its row");
    let row = parser.grid().row(1);
    assert!(row[0].flags.contains(CellFlags::WIDE), "lead cell is WIDE");
    assert!(row[1].flags.contains(CellFlags::WIDE_CONT), "trailing cell is WIDE_CONT");
}
