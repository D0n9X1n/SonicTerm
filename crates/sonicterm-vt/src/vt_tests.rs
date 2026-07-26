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
fn v120_parser_media_capture_shares_one_budget() {
    // The inventory recorded this as "independent parser limits only", and the
    // two capture slots do sit on separate structs with separate ceilings. But
    // beginning any escape family cancels a capture already in flight, so they
    // are alternatives rather than addends: the parser cannot hold both.
    //
    // Measured before writing this — feeding an 8 MiB DCS capture and then an
    // 8 MiB APC capture grows resident memory once, not twice. So the budget
    // that needs guarding is not their sum but the exclusivity that keeps a sum
    // from arising, and that is what this asserts.
    let mut parser = Parser::new(Grid::new(80, 24));
    assert_eq!(parser.live_capture_count(), 0, "a fresh parser holds no capture");
    assert_eq!(parser.retained_amount().items, 0);

    // Open a DCS Sixel capture and leave it unterminated.
    let mut dcs = b"\x1bPq".to_vec();
    dcs.extend(std::iter::repeat_n(b'#', 64 * 1024));
    parser.advance(&dcs);
    let during_dcs = parser.retained_amount();
    assert_eq!(during_dcs.items, 1, "one capture is live");
    assert!(during_dcs.bytes >= 64 * 1024, "the capture reports what it is holding");

    // Open an APC capture while the DCS one is still open.
    let mut apc = b"\x1b_G".to_vec();
    apc.extend(std::iter::repeat_n(b'A', 64 * 1024));
    parser.advance(&apc);

    // Measured: the escape terminates the capture in flight, and vte's DCS
    // unhook drains it into a completed media event before the next family
    // begins. Exclusivity is therefore upheld by the vendored state machine
    // rather than by anything here, which is worth stating plainly — this
    // asserts the property holds, not that SonicTerm is what holds it.
    assert!(
        parser.live_capture_count() <= 1,
        "two captures live at once is the state a summed budget would guard, \
         and it must stay unreachable; saw {}",
        parser.live_capture_count()
    );

    // Terminating returns the parser to holding nothing.
    parser.advance(b"\x1b\\");
    assert_eq!(parser.live_capture_count(), 0, "a terminated capture is released");
    assert_eq!(
        parser.retained_amount().items,
        0,
        "retention falls to zero once no capture is in flight"
    );
}

#[test]
fn parser_retention_excludes_the_grid_it_owns() {
    // A pane composes the parser's figure with the grid's rather than one
    // restating the other. Filling the grid must not change what the parser
    // reports holding, or a pane charging both would double count every cell.
    let mut parser = Parser::new(Grid::new(80, 24));
    let empty = parser.retained_amount();

    parser.advance(b"hello world");
    for _ in 0..200 {
        parser.advance(b"filling the scrollback with rows\r\n");
    }

    assert!(parser.grid().retained_amount().bytes > 0, "the grid holds cells");
    assert_eq!(
        parser.retained_amount(),
        empty,
        "grid growth belongs to the grid's figure, not the parser's"
    );
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

/// The three retention seams a pane must add up, and the fact that none of
/// them restates another.
///
/// `Parser::retained_amount`, `Grid::retained_amount`, and
/// `HyperlinkRegistry::retained_bytes` each report only what they own. That
/// is deliberate — the registry's own docs say it is "exposed so a governor
/// charges what the registry actually admits rather than a second estimate of
/// it" — and it means the pane owner charges the sum of all three.
///
/// The failure mode this guards is silent under-charging: an integrator who
/// composes grid + parser and stops there misses up to
/// `MAX_HYPERLINK_METADATA_BYTES` (8 MiB) per parser, because hyperlink
/// metadata hangs off the parser rather than being reported by it. Measured
/// here: 200 interned links retain ~18 KB that neither of the other two
/// figures moves by a single byte.
#[test]
fn the_three_retention_seams_are_disjoint_and_must_be_summed() {
    let mut parser = Parser::new(Grid::new(80, 24));

    let parser_before = parser.retained_amount().bytes;
    let grid_before = parser.grid().retained_amount().bytes;
    let links_before = parser.hyperlinks().retained_bytes();
    assert_eq!(links_before, 0, "a fresh parser interns no hyperlinks");

    // OSC 8 hyperlinks: ESC ] 8 ; id=N ; URI ESC \ ... ESC ] 8 ; ; ESC \
    for index in 0..200u32 {
        let sequence = format!(
            "\x1b]8;id={index};https://example.com/a-fairly-long-path/{index}\x1b\\link\x1b]8;;\x1b\\"
        );
        parser.advance(sequence.as_bytes());
    }

    let parser_after = parser.retained_amount().bytes;
    let links_after = parser.hyperlinks().retained_bytes();

    assert_eq!(parser.hyperlinks().len(), 200, "every link interned");
    assert!(links_after > 0, "interning links must retain bytes");

    // The parser does not restate hyperlink bytes. If it did, a pane summing
    // all three would charge them twice.
    assert_eq!(
        parser_after, parser_before,
        "Parser::retained_amount must not move when only hyperlinks are interned; \
         it reports capture and OSC accumulator bytes only"
    );

    // Neither does the grid, even though cells carry hyperlink ids.
    assert_eq!(
        parser.grid().retained_amount().bytes,
        grid_before,
        "Grid::retained_amount must exclude hyperlink metadata; the registry meters it"
    );

    // Therefore the registry's bytes are reachable from exactly one place, and
    // a pane that omits this term under-charges by that much.
    assert!(
        links_after >= 10_000,
        "200 links with long URIs should retain a meaningful amount, got {links_after}"
    );
}

/// A link written into the pane keeps working after the registry fills.
///
/// The registry is append-only in normal operation, so a pane that has seen
/// `MAX_HYPERLINKS` distinct links used to stop interning for the rest of the
/// session: every later OSC 8 rendered as plain text with no way to recover.
/// The links holding those slots are overwhelmingly ones whose cells scrolled
/// out of scrollback long ago.
///
/// This drives well past the cap through a small grid — so the early links
/// really are gone — and asserts the newest link is still resolvable, which
/// is what the user sees as a working hyperlink.
#[test]
fn a_pane_past_the_link_cap_still_renders_new_links() {
    use sonicterm_grid::hyperlink::MAX_HYPERLINKS;

    let mut grid = Grid::new(20, 4);
    grid.set_scrollback_limit(8);
    let mut parser = Parser::new(grid);

    for index in 0..MAX_HYPERLINKS + 64 {
        parser.advance(
            format!("\x1b]8;;https://example.com/{index}\x07link\x1b]8;;\x07\r\n").as_bytes(),
        );
    }

    parser.advance(b"\x1b]8;;https://example.com/final\x07final\x1b]8;;\x07");

    // Assert on the cell the final link was written to, not the parser's
    // open-span state: the close sequence above correctly clears the latter,
    // while the cell is what the renderer draws and the user clicks. Search
    // that row specifically — earlier rows still hold earlier links.
    let final_row = parser.grid().cursor.row;
    let hid = parser.grid().row(final_row).iter().find_map(|cell| cell.hyperlink()).unwrap_or_else(
        || {
            panic!(
                "a link after {MAX_HYPERLINKS} others must still reach a cell; registry holds {}",
                parser.hyperlinks().len()
            )
        },
    );
    assert_eq!(
        parser.hyperlinks().lookup(hid).map(|link| link.uri.as_str()),
        Some("https://example.com/final"),
        "the interned id must resolve to the URI the application sent"
    );
    assert!(
        parser.hyperlinks().len() <= MAX_HYPERLINKS,
        "reclamation must not push the registry past its own cap"
    );
}

/// Reclamation never frees a link the user can still see.
///
/// Both the visible screen and retained scrollback are live: a link scrolled
/// off screen but still within history is one the user reaches by scrolling
/// back, so freeing it would break a link that is still on display.
#[test]
fn reclamation_keeps_visible_scrollback_and_open_links() {
    use sonicterm_grid::hyperlink::MAX_HYPERLINKS;

    let mut grid = Grid::new(20, 4);
    grid.set_scrollback_limit(64);
    let mut parser = Parser::new(grid);

    parser.advance(b"\x1b]8;;https://example.com/scrolled\x07history\x1b]8;;\x07\r\n");
    let scrollback_link = parser
        .grid()
        .row(0)
        .iter()
        .find_map(|cell| cell.hyperlink())
        .expect("the first row must carry the link just written");

    // Push it into scrollback but keep it within the retained history.
    for _ in 0..6 {
        parser.advance(b"filler\r\n");
    }
    parser.advance(b"\x1b]8;;https://example.com/visible\x07visible\x1b]8;;\x07\r\n");
    let visible_link = parser
        .grid()
        .rows_iter()
        .flat_map(sonicterm_grid::grid::Row::iter)
        .find_map(|cell| cell.hyperlink())
        .expect("a visible row must carry the second link");

    // Force the registry to its cap so the next intern triggers a sweep.
    for index in 0..MAX_HYPERLINKS + 8 {
        parser.advance(format!("\x1b]8;id=f{index};https://example.com/f{index}\x07").as_bytes());
    }

    for (label, hid) in [("scrollback", scrollback_link), ("visible", visible_link)] {
        assert!(
            parser.hyperlinks().lookup(hid).is_some(),
            "reclamation freed the {label} link, which is still referenced"
        );
    }
}

/// A full-screen program's links do not survive as garbage, but the primary
/// screen behind it does.
///
/// While the alt screen is active the grid holds the *primary* the user will
/// return to. A sweep that walked only the live screen would free every link
/// on the screen behind it, and they would come back dead the moment the
/// program exits.
#[test]
fn reclamation_reaches_the_screen_behind_the_alt_buffer() {
    use sonicterm_grid::hyperlink::MAX_HYPERLINKS;

    let mut parser = Parser::new(Grid::new(20, 4));
    parser.advance(b"\x1b]8;;https://example.com/primary\x07primary\x1b]8;;\x07");
    let primary_link = parser
        .grid()
        .row(0)
        .iter()
        .find_map(|cell| cell.hyperlink())
        .expect("the primary screen must carry the link");

    parser.advance(b"\x1b[?1049h");
    assert!(parser.grid().is_alt(), "precondition: the alt screen is active");

    for index in 0..MAX_HYPERLINKS + 8 {
        parser.advance(format!("\x1b]8;id=a{index};https://example.com/a{index}\x07").as_bytes());
    }

    assert!(
        parser.hyperlinks().lookup(primary_link).is_some(),
        "a sweep during alt-screen use freed a link the primary screen still shows"
    );

    parser.advance(b"\x1b[?1049l");
    assert!(!parser.grid().is_alt());
    assert_eq!(
        parser.grid().row(0).iter().find_map(|cell| cell.hyperlink()),
        Some(primary_link),
        "the restored primary screen must still reference its link"
    );
    assert!(
        parser.hyperlinks().lookup(primary_link).is_some(),
        "the restored link must still resolve after leaving the alt screen"
    );
}

/// RIS drops every interned link.
///
/// A full reset erases every screen and the alt buffer, so no cell can still
/// reference an interned link. Retaining them would leave a fresh terminal
/// carrying the previous session's entire link set against its cap.
#[test]
fn ris_clears_the_hyperlink_registry() {
    let mut parser = Parser::new(Grid::new(20, 4));
    for index in 0..64 {
        parser
            .advance(format!("\x1b]8;;https://example.com/{index}\x07link\x1b]8;;\x07").as_bytes());
    }
    assert!(!parser.hyperlinks().is_empty(), "precondition: links are interned");

    parser.advance(b"\x1bc");

    assert!(parser.hyperlinks().is_empty(), "RIS must drop every interned link");
    assert_eq!(parser.hyperlinks().retained_bytes(), 0);
}

/// Links work again as soon as scrollback frees the registry.
///
/// After a sweep that frees nothing — every interned link genuinely still on
/// screen — the parser backs off before scanning again. That guard is right in
/// principle: rescanning per link when nothing is reclaimable turns a dead
/// feature into a stall.
///
/// But the backoff was set on a snapshot of grid state and never invalidated
/// when that state changed. Scrolling every link out of scrollback makes the
/// whole registry garbage, and the next links would still take the skip branch
/// — rendering as plain text despite megabytes being reclaimable. It
/// self-heals after the counter drains, which is exactly what makes it get
/// reported as "hyperlinks stop working sometimes" and resist reproduction.
#[test]
fn links_recover_as_soon_as_scrollback_frees_the_registry() {
    use sonicterm_grid::hyperlink::MAX_HYPERLINKS;

    let mut grid = Grid::new(20, 4);
    // Deep enough to hold every link on screen, so the first sweep frees zero.
    grid.set_scrollback_limit(MAX_HYPERLINKS + 64);
    let mut parser = Parser::new(grid);

    for index in 0..MAX_HYPERLINKS {
        parser.advance(
            format!("\x1b]8;;https://example.com/{index}\x07link\x1b]8;;\x07\r\n").as_bytes(),
        );
    }

    // The registry is full and everything in it is still reachable, so the
    // next link fails and the parser backs off.
    parser.advance(b"\x1b]8;;https://example.com/wedged\x07x\x1b]8;;\x07");

    // Now shrink scrollback so every one of those links falls out of history.
    // Their cells are gone; the entire registry is garbage.
    parser.grid_mut().set_scrollback_limit(4);
    parser.advance(b"\r\n");

    // The very next link must work. It does not have to wait out a counter
    // that was set when the grid looked completely different.
    parser.advance(b"\x1b]8;;https://example.com/after-scrollback\x07visible\x1b]8;;\x07");

    let row = parser.grid().cursor.row;
    let hid = parser
        .grid()
        .row(row)
        .iter()
        .find_map(|cell| cell.hyperlink())
        .expect("the link written after scrollback shrank must reach a cell");
    assert_eq!(
        parser.hyperlinks().lookup(hid).map(|link| link.uri.as_str()),
        Some("https://example.com/after-scrollback"),
        "a link must work as soon as scrollback frees the registry, not after \
         the backoff counter happens to drain"
    );
}
