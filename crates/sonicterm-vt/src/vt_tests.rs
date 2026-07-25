use super::{MediaProtocol, Parser, VtEvent, MAX_ESCAPE_SEQUENCE_BYTES};
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
fn full_reply_queue_drops_excess_without_blocking_parser() {
    let (tx, rx) = crossbeam_channel::bounded(1);
    let mut parser = Parser::new_with_reply(Grid::new(80, 24), tx);

    parser.advance(b"\x1b[5n");
    parser.advance(b"\x1b[5n");

    assert_eq!(rx.len(), 1);
    assert_eq!(rx.try_recv().expect("first reply retained"), b"\x1b[0n");
}

#[test]
fn oversized_osc_is_discarded_without_unbounded_buffering() {
    let mut parser = Parser::new(Grid::new(80, 24));
    let mut payload = b"\x1b]8;;https://example.com/".to_vec();
    payload.extend(std::iter::repeat_n(b'a', MAX_ESCAPE_SEQUENCE_BYTES + 1));

    parser.advance(&payload);
    assert!(parser.discarding_oversized_escape);

    parser.advance(b"\x1b\\Z");
    assert!(!parser.discarding_oversized_escape);
    assert_eq!(parser.grid().row(0)[0].ch, 'Z');
}

#[test]
fn oversized_csi_resynchronizes_on_final_byte() {
    let mut parser = Parser::new(Grid::new(80, 24));
    let mut payload = b"\x1b[".to_vec();
    payload.extend(std::iter::repeat_n(b'1', MAX_ESCAPE_SEQUENCE_BYTES + 1));

    parser.advance(&payload);
    assert!(parser.discarding_oversized_escape);

    parser.advance(b"mZ");
    assert!(!parser.discarding_oversized_escape);
    assert_eq!(parser.grid().row(0)[0].ch, 'Z');
}

#[test]
fn can_and_sub_reset_escape_family_before_oversized_osc() {
    for cancel in [0x18, 0x1a] {
        let mut parser = Parser::new(Grid::new(80, 24));
        parser.advance(&[0x1b, b'[', b'1', cancel]);

        let mut payload = b"\x1b]0;".to_vec();
        payload.extend(std::iter::repeat_n(b'1', MAX_ESCAPE_SEQUENCE_BYTES + 1));
        payload.push(0x07);
        payload.push(b'Z');
        parser.advance(&payload);

        assert!(!parser.discarding_oversized_escape, "cancel byte {cancel:#x}");
        assert_eq!(parser.grid().row(0)[0].ch, 'Z', "cancel byte {cancel:#x}");
    }
}

#[test]
fn can_and_sub_cancel_sixel_without_emitting_media() {
    for cancel in [0x18, 0x1a] {
        let mut parser = Parser::new(Grid::new(80, 24));
        let events = parser.advance(&[0x1b, b'P', b'q', b'a', b'b', b'c', cancel, b'Z']);

        assert!(
            events.iter().all(|event| !matches!(event, VtEvent::Media(_))),
            "cancel byte {cancel:#x} emitted media"
        );
        assert_eq!(parser.grid().row(0)[0].ch, 'Z', "cancel byte {cancel:#x}");
    }
}

#[test]
fn overflow_triggering_osc_terminator_is_not_lost() {
    let mut parser = Parser::new(Grid::new(80, 24));
    let mut payload = b"\x1b]8;;".to_vec();
    payload.extend(std::iter::repeat_n(b'a', MAX_ESCAPE_SEQUENCE_BYTES - payload.len()));
    payload.push(0x07);
    payload.push(b'Z');

    parser.advance(&payload);

    assert!(!parser.discarding_oversized_escape);
    assert_eq!(parser.grid().row(0)[0].ch, 'Z');
}

#[test]
fn exact_limit_osc_resets_accounting_at_dispatch() {
    let mut parser = Parser::new(Grid::new(80, 24));
    let mut payload = b"\x1b]8;;".to_vec();
    payload.extend(std::iter::repeat_n(b'a', MAX_ESCAPE_SEQUENCE_BYTES - payload.len() - 2));
    payload.extend_from_slice(b"\x1b\\");
    assert_eq!(payload.len(), MAX_ESCAPE_SEQUENCE_BYTES);

    parser.advance(&payload);
    parser.advance(b"Z");

    assert!(!parser.discarding_oversized_escape);
    assert_eq!(parser.grid().row(0)[0].ch, 'Z');
}

#[test]
fn consecutive_ground_controls_do_not_consume_escape_budget() {
    let mut parser = Parser::new(Grid::new(80, 24));
    parser.advance(&vec![b'\n'; MAX_ESCAPE_SEQUENCE_BYTES + 1]);
    parser.advance(b"Z");

    assert!(!parser.discarding_oversized_escape);
    assert_eq!(parser.grid().row(parser.grid().rows - 1)[0].ch, 'Z');
}

#[test]
fn st_split_across_escape_limit_is_recognized() {
    let mut parser = Parser::new(Grid::new(80, 24));
    let mut payload = b"\x1b]8;;".to_vec();
    payload.extend(std::iter::repeat_n(b'a', MAX_ESCAPE_SEQUENCE_BYTES - payload.len() - 1));
    payload.push(0x1b);
    assert_eq!(payload.len(), MAX_ESCAPE_SEQUENCE_BYTES);

    parser.advance(&payload);
    parser.advance(b"\\Z");

    assert!(!parser.discarding_oversized_escape);
    assert_eq!(parser.grid().row(0)[0].ch, 'Z');
}

#[test]
fn large_sixel_uses_media_budget_not_generic_escape_limit() {
    let mut parser = Parser::new(Grid::new(80, 24));
    let mut payload = b"\x1bPq".to_vec();
    payload.extend(std::iter::repeat_n(b'?', MAX_ESCAPE_SEQUENCE_BYTES + 1));
    payload.extend_from_slice(b"\x1b\\");

    let events = parser.advance(&payload);

    let media = events
        .into_iter()
        .find_map(|event| match event {
            VtEvent::Media(media) => Some(media),
            _ => None,
        })
        .expect("large Sixel DCS should remain a media event");
    assert_eq!(media.protocol, MediaProtocol::Sixel);
    assert!(media.data.len() > MAX_ESCAPE_SEQUENCE_BYTES);
}

#[test]
#[ignore = "v120-invariant-baseline:v120_parser_media_capture_shares_one_budget:WP-VT"]
fn v120_parser_media_capture_shares_one_budget() {
    panic!("baseline invariant requires WP-VT compositional capture budget");
}

#[test]
fn rejected_hyperlink_open_emits_close_event() {
    let mut parser = Parser::new(Grid::new(80, 24));
    parser.advance(b"\x1b]8;;https://example.com\x1b\\");
    let mut rejected = b"\x1b]8;;https://example.com/".to_vec();
    rejected.extend(std::iter::repeat_n(b'x', sonicterm_grid::hyperlink::MAX_HYPERLINK_URI_BYTES));
    rejected.extend_from_slice(b"\x1b\\");

    let events = parser.advance(&rejected);

    assert!(events
        .iter()
        .any(|event| matches!(event, VtEvent::Hyperlink { uri, .. } if uri.is_empty())));
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
    // OSC 4; <i>; ? ST must reply with the seeded palette colour
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
    // vte 0.15 exposes only 16 split OSC params, which truncates a
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

// ---: insert-before must leave row content AND dirty state correct in a
// single pass. Reported symptom: "type 11, move before it, insert 0. → shows
// 0.1 not 0.11; the missing 1 reappears only on the next keystroke." That is a
// render-side staleness — these tests pin the VT/grid data layer as correct so
// the fix is scoped to the renderer, and guard against a future regression
// where an in-place edit fails to grow/dirty the row. ---

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
