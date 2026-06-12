
use super::Parser;
use sonicterm_grid::grid::{CellFlags, Grid};

fn row_text(parser: &Parser, row: u16) -> String {
    parser.grid().row(row).iter().map(|cell| cell.ch).collect()
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
fn dec_private_mode_1_toggles_application_cursor_keys() {
    let mut parser = Parser::new(Grid::new(8, 2));
    assert!(!parser.application_cursor_keys());

    parser.advance(b"\x1b[?1h");
    assert!(parser.application_cursor_keys());

    parser.advance(b"\x1b[?1l");
    assert!(!parser.application_cursor_keys());
}

#[test]
fn dec_private_mode_1000_toggles_mouse_tracking() {
    let mut parser = Parser::new(Grid::new(8, 2));
    assert!(!parser.mouse_tracking_enabled());

    parser.advance(b"\x1b[?1000h");
    assert!(parser.mouse_tracking_enabled());

    parser.advance(b"\x1b[?1000l");
    assert!(!parser.mouse_tracking_enabled());
}

#[test]
fn dec_private_mode_1002_1003_toggle_mouse_tracking() {
    let mut parser = Parser::new(Grid::new(8, 2));

    parser.advance(b"\x1b[?1002h");
    assert!(parser.mouse_tracking_enabled());
    parser.advance(b"\x1b[?1002l");
    assert!(!parser.mouse_tracking_enabled());

    parser.advance(b"\x1b[?1003h");
    assert!(parser.mouse_tracking_enabled());
    parser.advance(b"\x1b[?1003l");
    assert!(!parser.mouse_tracking_enabled());
}

#[test]
fn ris_resets_app_cursor_keys_and_mouse_tracking() {
    let mut parser = Parser::new(Grid::new(8, 2));
    parser.advance(b"\x1b[?1h\x1b[?1000h");
    assert!(parser.application_cursor_keys());
    assert!(parser.mouse_tracking_enabled());

    parser.advance(b"\x1bc");

    assert!(!parser.application_cursor_keys());
    assert!(!parser.mouse_tracking_enabled());
}

#[test]
fn kitty_keyboard_push_sets_flags() {
    let mut parser = Parser::new(Grid::new(8, 2));
    assert_eq!(parser.kitty_keyboard_flags(), 0);

    // CSI > 1 u — push flags = 1 (disambiguate escape codes).
    parser.advance(b"\x1b[>1u");
    assert_eq!(parser.kitty_keyboard_flags(), 1);
}

#[test]
fn kitty_keyboard_pop_restores_previous_flags() {
    let mut parser = Parser::new(Grid::new(8, 2));
    parser.advance(b"\x1b[>1u");
    parser.advance(b"\x1b[>5u");
    assert_eq!(parser.kitty_keyboard_flags(), 5);

    // CSI < u — pop one entry (default count 1).
    parser.advance(b"\x1b[<u");
    assert_eq!(parser.kitty_keyboard_flags(), 1);

    // Pop the last entry back to legacy (0).
    parser.advance(b"\x1b[<u");
    assert_eq!(parser.kitty_keyboard_flags(), 0);

    // Popping an empty stack is a no-op, not a panic.
    parser.advance(b"\x1b[<u");
    assert_eq!(parser.kitty_keyboard_flags(), 0);
}

#[test]
fn kitty_keyboard_pop_count_pops_multiple() {
    let mut parser = Parser::new(Grid::new(8, 2));
    parser.advance(b"\x1b[>1u");
    parser.advance(b"\x1b[>2u");
    parser.advance(b"\x1b[>4u");
    assert_eq!(parser.kitty_keyboard_flags(), 4);

    // CSI < 2 u — pop two entries.
    parser.advance(b"\x1b[<2u");
    assert_eq!(parser.kitty_keyboard_flags(), 1);
}

#[test]
fn kitty_keyboard_set_replaces_top() {
    let mut parser = Parser::new(Grid::new(8, 2));
    // CSI = flags u with an empty stack pushes the active set.
    parser.advance(b"\x1b[=3u");
    assert_eq!(parser.kitty_keyboard_flags(), 3);

    // CSI = 5 ; 1 u — mode 1 (default) replaces the top.
    parser.advance(b"\x1b[=5;1u");
    assert_eq!(parser.kitty_keyboard_flags(), 5);

    // CSI = 2 ; 2 u — mode 2 ORs in the new bits.
    parser.advance(b"\x1b[=2;2u");
    assert_eq!(parser.kitty_keyboard_flags(), 7);

    // CSI = 1 ; 3 u — mode 3 clears the given bits.
    parser.advance(b"\x1b[=1;3u");
    assert_eq!(parser.kitty_keyboard_flags(), 6);
}

#[test]
fn kitty_keyboard_stack_depth_is_capped() {
    let mut parser = Parser::new(Grid::new(8, 2));
    // Push far more than the cap; flags must stay valid and the stack must
    // not grow without bound.
    for _ in 0..100 {
        parser.advance(b"\x1b[>1u");
    }
    assert_eq!(parser.kitty_keyboard_flags(), 1);
}

#[test]
fn kitty_keyboard_query_reports_current_flags() {
    let (tx, rx) = crossbeam_channel::unbounded();
    let mut parser = Parser::new_with_reply(Grid::new(8, 2), tx);

    // Query with no flags pushed → reply CSI ? 0 u.
    parser.advance(b"\x1b[?u");
    assert_eq!(rx.try_recv().unwrap(), b"\x1b[?0u".to_vec());

    // Push flags = 1, then query → reply CSI ? 1 u.
    parser.advance(b"\x1b[>1u");
    parser.advance(b"\x1b[?u");
    assert_eq!(rx.try_recv().unwrap(), b"\x1b[?1u".to_vec());
}

#[test]
fn ris_resets_kitty_keyboard_flags() {
    let mut parser = Parser::new(Grid::new(8, 2));
    parser.advance(b"\x1b[>1u");
    assert_eq!(parser.kitty_keyboard_flags(), 1);

    parser.advance(b"\x1bc");
    assert_eq!(parser.kitty_keyboard_flags(), 0);
}

#[test]
fn osc4_palette_query_replies_with_seeded_color() {
    // #661: OSC 4 ; <i> ; ? ST must reply with the seeded palette colour
    // so CLIs like Copilot can read the full colour set. Reply format is
    // `ESC ] 4 ; <i> ; rgb:RRRR/GGGG/BBBB ST` with 16-bit channels.
    let (tx, rx) = crossbeam_channel::unbounded();
    let mut parser = Parser::new_with_reply(Grid::new(8, 2), tx);
    parser.set_theme_palette_color(1, 0xAA, 0xBB, 0xCC); // ANSI red slot

    // BEL-terminated query.
    parser.advance(b"\x1b]4;1;?\x07");
    assert_eq!(rx.try_recv().unwrap(), b"\x1b]4;1;rgb:aaaa/bbbb/cccc\x07".to_vec());

    // ST-terminated query echoes an ST terminator.
    parser.advance(b"\x1b]4;1;?\x1b\\");
    assert_eq!(rx.try_recv().unwrap(), b"\x1b]4;1;rgb:aaaa/bbbb/cccc\x1b\\".to_vec());
}

#[test]
fn osc4_unseeded_slot_is_silent() {
    // A slot we were never told about must NOT reply (don't lie about a
    // colour we don't have).
    let (tx, rx) = crossbeam_channel::unbounded();
    let mut parser = Parser::new_with_reply(Grid::new(8, 2), tx);
    parser.advance(b"\x1b]4;5;?\x07");
    assert!(rx.try_recv().is_err(), "unseeded slot must not reply");
}

#[test]
fn osc4_multi_pair_query_replies_per_index() {
    // xterm allows several `index ; spec` pairs in one OSC 4 — each `?`
    // gets its own reply, in order.
    let (tx, rx) = crossbeam_channel::unbounded();
    let mut parser = Parser::new_with_reply(Grid::new(8, 2), tx);
    parser.set_theme_palette_color(0, 0x10, 0x20, 0x30);
    parser.set_theme_palette_color(15, 0xF0, 0xE0, 0xD0);

    parser.advance(b"\x1b]4;0;?;15;?\x07");
    assert_eq!(rx.try_recv().unwrap(), b"\x1b]4;0;rgb:1010/2020/3030\x07".to_vec());
    assert_eq!(rx.try_recv().unwrap(), b"\x1b]4;15;rgb:f0f0/e0e0/d0d0\x07".to_vec());
}

#[test]
fn osc4_full_16_color_batch_query_replies_for_every_seeded_slot() {
    // #667: vte 0.15 exposes only 16 split OSC params, which truncates a
    // full `OSC 4;0;?;...;15;? ST` query. SonicTerm's parser keeps enough
    // raw OSC4 state to answer every seeded pair.
    let (tx, rx) = crossbeam_channel::unbounded();
    let mut parser = Parser::new_with_reply(Grid::new(8, 2), tx);
    for idx in 0..16u8 {
        parser.set_theme_palette_color(idx, idx, idx + 0x10, idx + 0x20);
    }

    let mut query = String::from("\x1b]4");
    for idx in 0..16u8 {
        query.push_str(&format!(";{idx};?"));
    }
    query.push_str("\x1b\\");
    parser.advance(query.as_bytes());

    for idx in 0..16u8 {
        let expected = format!(
            "\x1b]4;{idx};rgb:{idx:02x}{idx:02x}/{:02x}{:02x}/{:02x}{:02x}\x1b\\",
            idx + 0x10,
            idx + 0x10,
            idx + 0x20,
            idx + 0x20,
        )
        .into_bytes();
        assert_eq!(rx.try_recv().unwrap(), expected);
    }
    assert!(rx.try_recv().is_err(), "OSC4 batch must not produce duplicate replies");

    parser.advance(b"Z");
    assert_eq!(row_text(&parser, 0), "Z       ");
}
