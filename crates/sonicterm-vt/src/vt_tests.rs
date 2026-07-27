use super::{
    MediaCapture, MediaProtocol, Parser, VtEvent, CAPTURE_FLOOR_POOL_BYTES,
    CAPTURE_GROWTH_POOL_BYTES, GUARANTEED_CONCURRENT_CAPTURES, LIVE_MEDIA_CAPTURES,
    MAX_ESCAPE_SEQUENCE_BYTES, MAX_MEDIA_PAYLOAD_BYTES, MAX_PROCESS_CAPTURE_STAGING_BYTES,
    MIN_CAPTURE_STAGING_BYTES,
};
use sonicterm_grid::grid::{CellFlags, Grid};
use std::sync::atomic::Ordering;

/// Serialises tests that depend on how much of the staging pools is free.
///
/// The pools are process-wide, so a test holding captures changes what a
/// concurrently-running test is admitted for. Tests that merely open a capture
/// do not need this; tests that assert *whether* a capture was admitted do.
static POOLS: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
    let _serialised = POOLS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
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
    let _serialised = POOLS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
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

    // The grid moves, and must: a linked cell allocates a 40-byte
    // `Box<FatAttributes>` that the grid owns. What the registry meters is the
    // URI *strings* — `(id + uri) * 2` — which is a different allocation. The
    // two are disjoint, so both are charged exactly once.
    //
    // This assertion previously required the grid figure not to move at all,
    // using that as a proxy for "no double counting". The proxy was wrong in
    // the direction that matters: it also holds when the grid fails to count
    // something it genuinely owns, which is the defect it was meant to
    // exclude. Measured before the fix: 1.85 MiB reported against 3.09 MiB
    // held on a populated scrollback.
    let grid_after = parser.grid().retained_amount().bytes;
    assert!(
        grid_after > grid_before,
        "linked cells allocate grid-owned attribute boxes, which the grid must count"
    );

    // Disjointness, asserted directly rather than through a proxy: the grid's
    // increase is bounded by what the boxes can cost, so it cannot be
    // restating the registry's string bytes.
    let linked_cells: usize = parser
        .grid()
        .rows_iter()
        .chain(parser.grid().scrollback_iter())
        .map(|row| row.iter().filter(|cell| cell.hyperlink().is_some()).count())
        .sum();
    let max_box_bytes = linked_cells * std::mem::size_of::<sonicterm_types::FatAttributes>();
    assert!(
        grid_after - grid_before <= max_box_bytes,
        "the grid's increase ({}) exceeds what {linked_cells} attribute boxes can cost \
         ({max_box_bytes}) — it is restating registry bytes",
        grid_after - grid_before
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

/// An oversized URI does not trigger a reclamation sweep.
///
/// The parser sweeps the grid and retries when the registry refuses a link.
/// That is right for a full registry and wrong for an oversized URI, which is
/// refused by a size check no sweep can change — an `O(visible + scrollback)`
/// walk on the VT hot path to reach the same answer, once per link.
///
/// Observed through the registry's contents rather than by counting sweeps:
/// a sweep that ran would free the unreferenced links this test parks in the
/// registry, so their survival is the evidence it did not run.
#[test]
fn an_oversized_uri_does_not_trigger_a_reclamation_sweep() {
    let mut grid = Grid::new(20, 4);
    grid.set_scrollback_limit(2);
    let mut parser = Parser::new(grid);

    // Park links in the registry, then scroll them out so a sweep would free
    // them. They are unreferenced garbage from here on.
    for index in 0..40 {
        parser.advance(
            format!("\x1b]8;;https://example.com/{index}\x07link\x1b]8;;\x07\r\n").as_bytes(),
        );
    }
    for _ in 0..20 {
        parser.advance(b"scroll\r\n");
    }
    let retained_before = parser.hyperlinks().len();
    assert!(retained_before > 0, "precondition: the registry holds reclaimable links");

    // An oversized URI: rejected on size, and no sweep can change that.
    let huge = "x".repeat(sonicterm_grid::hyperlink::MAX_HYPERLINK_URI_BYTES + 1);
    parser.advance(format!("\x1b]8;;{huge}\x07text\x1b]8;;\x07").as_bytes());

    assert_eq!(
        parser.hyperlinks().len(),
        retained_before,
        "an oversized URI must not trigger a sweep: the registry's contents changed, \
         so the parser scanned the whole grid to reach a rejection the size check had \
         already decided"
    );
}

/// A full registry still triggers a sweep, so the skip is targeted.
///
/// Without this the test above could pass by disabling reclamation entirely,
/// which would reintroduce the permanent hyperlink wedge.
#[test]
fn a_full_registry_still_triggers_a_reclamation_sweep() {
    use sonicterm_grid::hyperlink::MAX_HYPERLINKS;

    let mut grid = Grid::new(20, 4);
    grid.set_scrollback_limit(2);
    let mut parser = Parser::new(grid);

    for index in 0..MAX_HYPERLINKS {
        parser.advance(
            format!("\x1b]8;;https://example.com/{index}\x07l\x1b]8;;\x07\r\n").as_bytes(),
        );
    }
    let full = parser.hyperlinks().len();
    assert_eq!(full, MAX_HYPERLINKS, "precondition: the registry is at its count cap");

    // A normal link against a full registry must sweep, free the scrolled-away
    // entries, and succeed.
    parser.advance(b"\x1b]8;;https://example.com/after\x07visible\x1b]8;;\x07");

    assert!(
        parser.hyperlinks().len() < full,
        "a retryable rejection must still sweep: {} entries retained, expected fewer \
         than {full}",
        parser.hyperlinks().len()
    );
    let row = parser.grid().cursor.row;
    assert!(
        parser.grid().row(row).iter().any(|cell| cell.hyperlink().is_some()),
        "and the link must end up on a cell"
    );
}

// ---------------------------------------------------------------------------
// Capture staging budget
//
// A capture is staging, not retained: it lives between an APC/DCS introducer
// and its terminator. What makes it worth bounding is that the terminator is
// not guaranteed to arrive — a stalled transfer pins its buffer until the pane
// dies, and no eviction pass can reclaim it.
// ---------------------------------------------------------------------------

/// The pools must sum to the ceiling exactly.
///
/// The ceiling is only a ceiling if nothing can be handed out that is not
/// drawn from one of the two pools. Asserted as arithmetic over the
/// constants — the heap-truth integration tests are what check that the code
/// actually obeys them.
#[test]
fn the_pools_sum_to_the_process_ceiling() {
    assert_eq!(
        CAPTURE_FLOOR_POOL_BYTES + CAPTURE_GROWTH_POOL_BYTES,
        MAX_PROCESS_CAPTURE_STAGING_BYTES,
        "staging handed out from pools that do not sum to the ceiling is not bounded by it"
    );
}

/// The growth pool must let one capture reach the per-capture maximum.
///
/// A lone pane receiving a large image is the common case and the one the
/// ceiling must not touch. If growth were smaller than the climb from the
/// floor to the maximum, no capture could ever reach the maximum and
/// `MAX_MEDIA_PAYLOAD_BYTES` would be a number no code path can produce.
#[test]
fn the_growth_pool_covers_one_capture_climbing_to_the_maximum() {
    assert_eq!(
        CAPTURE_GROWTH_POOL_BYTES,
        MAX_MEDIA_PAYLOAD_BYTES - MIN_CAPTURE_STAGING_BYTES,
        "the growth pool must be exactly one capture's climb from the floor to the maximum"
    );
}

/// The guarantee must be the floor pool divided by the floor.
///
/// Derived rather than chosen, so the promise cannot drift from the pool that
/// backs it. A guarantee larger than the pool would be a promise the pools
/// cannot keep; smaller would be leaving panes unrendered for no reason.
#[test]
fn the_guarantee_is_derived_from_the_floor_pool() {
    assert_eq!(
        GUARANTEED_CONCURRENT_CAPTURES * MIN_CAPTURE_STAGING_BYTES,
        CAPTURE_FLOOR_POOL_BYTES,
        "the guaranteed count must be exactly what the floor pool can floor"
    );
}

/// The guarantee must cover a plausible session.
///
/// A change that held the ceiling by guaranteeing one or two panes would
/// satisfy every bound assertion in the suite while making the terminal
/// useless for the case the floor exists to serve. Compile-time because both
/// sides are constants.
const _: () = assert!(
    GUARANTEED_CONCURRENT_CAPTURES >= 8,
    "the staging pools guarantee too few concurrent captures to cover a plausible \
     working session"
);

/// Every capture inside the guarantee is admitted; the next one is refused.
///
/// This is the admission boundary itself. The old policy had no boundary — it
/// divided a share and clamped it at a floor, so the `N + 1`th capture was
/// admitted at the floor exactly like the first, and the sum grew without
/// limit. What replaced it must actually stop.
#[test]
fn captures_are_admitted_up_to_the_guarantee_and_refused_past_it() {
    let _serialised = POOLS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

    let admitted: Vec<MediaCapture> = (0..GUARANTEED_CONCURRENT_CAPTURES)
        .map(|_| MediaCapture::new(MediaProtocol::Kitty, String::new()))
        .collect();

    for (index, capture) in admitted.iter().enumerate() {
        assert!(
            capture.admitted(),
            "capture {index} is inside the guarantee of {GUARANTEED_CONCURRENT_CAPTURES} and \
             must be admitted"
        );
    }

    let refused = MediaCapture::new(MediaProtocol::Kitty, String::new());
    assert!(
        !refused.admitted(),
        "the capture past the guarantee must be refused, or the ceiling is not a ceiling"
    );

    drop(refused);
    drop(admitted);
}

/// A refused capture must keep no bytes at all.
///
/// Admission is what bounds the total, so a refused capture that still
/// accumulated would make the bound decorative.
#[test]
fn a_refused_capture_stages_nothing() {
    let _serialised = POOLS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

    let held: Vec<MediaCapture> = (0..GUARANTEED_CONCURRENT_CAPTURES)
        .map(|_| MediaCapture::new(MediaProtocol::Kitty, String::new()))
        .collect();

    let mut refused = MediaCapture::new(MediaProtocol::Kitty, String::new());
    assert!(!refused.admitted(), "precondition: the pool is committed");

    for byte in b"a-payload-that-must-not-be-staged" {
        refused.append_byte(*byte);
    }

    assert_eq!(refused.data.len(), 0, "a refused capture must keep no bytes");
    assert_eq!(refused.retained_bytes(), 0, "and hold no allocation");
    assert!(refused.truncated, "and know that it dropped what it was given");

    drop(refused);
    drop(held);
}

/// Releasing a capture must return its bytes to the pools.
///
/// A pool that is never returned to would refuse everything after the first
/// burst of captures, which is a permanent degradation rather than a bound.
#[test]
fn releasing_a_capture_returns_its_pool_bytes() {
    let _serialised = POOLS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

    let held: Vec<MediaCapture> = (0..GUARANTEED_CONCURRENT_CAPTURES)
        .map(|_| MediaCapture::new(MediaProtocol::Kitty, String::new()))
        .collect();
    assert!(
        !MediaCapture::new(MediaProtocol::Kitty, String::new()).admitted(),
        "precondition: the pool is committed"
    );

    drop(held);

    let after = MediaCapture::new(MediaProtocol::Kitty, String::new());
    assert!(after.admitted(), "a capture must be admitted once the pool is released");
}

/// A capture must grow to the per-capture maximum when the pool allows.
///
/// The growth path is what keeps the common case whole, and it runs only when
/// a capture fills its floor. A fix that bounded the total by never growing
/// would pass every ceiling test and quietly cap every image at the floor.
#[test]
fn an_admitted_capture_grows_to_the_per_capture_maximum() {
    let _serialised = POOLS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

    let mut capture = MediaCapture::new(MediaProtocol::Kitty, String::new());
    assert!(capture.admitted(), "precondition: admitted into a free pool");

    for _ in 0..MAX_MEDIA_PAYLOAD_BYTES {
        capture.append_byte(b'A');
    }

    assert_eq!(
        capture.data.len(),
        MAX_MEDIA_PAYLOAD_BYTES,
        "a capture growing alone must reach the per-capture maximum"
    );
    assert!(!capture.truncated, "and must not report truncation at the maximum");
}

/// Growth must stop at the per-capture maximum.
///
/// The floor keeps small images whole; this keeps one capture from taking the
/// whole growth pool and then continuing past the cap the app charges panes
/// against.
#[test]
fn growth_stops_at_the_per_capture_maximum() {
    let _serialised = POOLS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

    let mut capture = MediaCapture::new(MediaProtocol::Kitty, String::new());
    for _ in 0..(MAX_MEDIA_PAYLOAD_BYTES + 4096) {
        capture.append_byte(b'A');
    }

    assert_eq!(capture.data.len(), MAX_MEDIA_PAYLOAD_BYTES, "growth must stop at the maximum");
    assert!(capture.truncated, "and past it the capture must know it was cut");
    assert!(
        capture.retained_bytes() <= MAX_MEDIA_PAYLOAD_BYTES + capture.metadata.capacity(),
        "the allocation must not exceed the budget the pools granted: held {}",
        capture.retained_bytes()
    );
}

/// Captures alive at the same time must contend for the same pools.
///
/// This is what fails if the reservation is never taken: with nothing charged
/// to the pools, every capture is admitted and claims the full 16 MiB, which
/// is exactly the 20 × 16 MiB = 320 MiB composition this package exists to
/// close.
#[test]
fn concurrent_captures_contend_for_the_same_pools() {
    let _serialised = POOLS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

    let held: Vec<MediaCapture> =
        (0..32).map(|_| MediaCapture::new(MediaProtocol::Kitty, String::new())).collect();

    let admitted = held.iter().filter(|capture| capture.admitted()).count();
    assert_eq!(
        admitted, GUARANTEED_CONCURRENT_CAPTURES,
        "32 concurrent captures must be admitted up to the guarantee and no further"
    );

    let staged: usize = held.iter().map(MediaCapture::retained_bytes).sum();
    assert!(
        staged <= MAX_PROCESS_CAPTURE_STAGING_BYTES,
        "32 concurrent captures hold {staged} bytes against a ceiling of \
         {MAX_PROCESS_CAPTURE_STAGING_BYTES}"
    );

    drop(held);
}

/// Ending a capture must return its share to everyone else.
///
/// This is what fails if the charge is taken but never released — including if
/// `into_kitty_event`/`into_event` stopped dropping the guard when they move
/// the payload out. A leak there is invisible in any single-capture test: the
/// budget only misbehaves once the phantom count accumulates.
#[test]
fn a_finished_capture_returns_its_share() {
    let _serialised = POOLS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let baseline = LIVE_MEDIA_CAPTURES.load(Ordering::Relaxed);

    // Drive real captures to completion through the parser, so the release
    // path under test is the production one rather than a bare `drop`.
    let mut parser = Parser::new(Grid::new(80, 24));
    for _ in 0..64 {
        parser.advance(b"\x1b_Gf=100;payload\x1b\\");
    }
    assert_eq!(parser.live_capture_count(), 0, "precondition: no capture left in flight");

    let after = LIVE_MEDIA_CAPTURES.load(Ordering::Relaxed);
    assert!(
        after <= baseline,
        "64 completed captures leaked {} charges; a capture that ends must release its \
         share or the budget decays permanently",
        after.saturating_sub(baseline)
    );
}

/// Arriving captures must be bounded by what the pools have left.
///
/// The measurement that motivated this package fed 20 parsers an unterminated
/// APC introducer plus 16 MiB and found 320 MiB pinned, every parser
/// individually compliant. This is that measurement, asserted.
///
/// Each parser is fed once and then never again — the stalled shape, which is
/// the one no growth policy can fix, because a stalled capture never runs
/// again to be re-budgeted. Admission is what bounds it: a capture that the
/// pools cannot floor is refused at birth rather than admitted at a size that
/// would put the total over.
#[test]
fn arriving_captures_are_bounded_by_what_the_pools_have_left() {
    let _serialised = POOLS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    const PANES: usize = 20;

    let mut parsers: Vec<Parser> = (0..PANES).map(|_| Parser::new(Grid::new(80, 24))).collect();

    // An APC introducer with no terminator: the stalled-transfer shape.
    let mut chunk = Vec::with_capacity(MAX_MEDIA_PAYLOAD_BYTES + 3);
    chunk.extend_from_slice(b"\x1b_G");
    chunk.resize(MAX_MEDIA_PAYLOAD_BYTES + 3, b'A');

    for parser in parsers.iter_mut() {
        parser.advance(&chunk);
    }

    // Every parser is still mid-capture — otherwise any total below would be
    // small for the wrong reason.
    for (idx, parser) in parsers.iter().enumerate() {
        assert_eq!(parser.live_capture_count(), 1, "parser {idx} must still hold its capture");
    }

    // Against the ceiling itself, not against the naive product. `total <
    // MAX_MEDIA_PAYLOAD_BYTES * PANES` is satisfied by any policy that divides
    // at all, including one whose sum grows without limit — it was what this
    // assertion said while the process held 80 MiB against a stated 64 MiB.
    let total: usize = parsers.iter().map(|p| p.retained_amount().bytes).sum();
    assert!(
        total <= MAX_PROCESS_CAPTURE_STAGING_BYTES,
        "{PANES} stalled captures hold {} MiB against a ceiling of {} MiB",
        total / (1024 * 1024),
        MAX_PROCESS_CAPTURE_STAGING_BYTES / (1024 * 1024)
    );

    // And the ones past the guarantee took nothing at all, rather than a floor
    // the ceiling could not back.
    let staged = parsers.iter().filter(|p| p.retained_amount().bytes > 0).count();
    assert!(
        staged <= GUARANTEED_CONCURRENT_CAPTURES,
        "{staged} captures were staged, but the pools can only guarantee \
         {GUARANTEED_CONCURRENT_CAPTURES}"
    );
}

/// A stalled capture is reclaimed by the host, not by the parser.
///
/// The parser has no clock and cannot tell a stalled transfer from a slow one.
/// It exposes progress so the host can tell, and cancellation so the host can
/// act. Neither half is useful alone, so both are asserted together.
#[test]
fn a_stalled_capture_is_visible_as_progress_and_releasable_by_cancel() {
    let _serialised = POOLS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut parser = Parser::new(Grid::new(80, 24));

    let mut chunk = Vec::with_capacity(MAX_MEDIA_PAYLOAD_BYTES + 3);
    chunk.extend_from_slice(b"\x1b_G");
    chunk.resize(MAX_MEDIA_PAYLOAD_BYTES + 3, b'A');
    parser.advance(&chunk);

    assert_eq!(parser.live_capture_count(), 1, "precondition: a capture is in flight");
    let held = parser.retained_amount().bytes;
    assert!(held > 0, "precondition: it is holding staging");

    // Two samples with no bytes in between: this is how a host distinguishes a
    // stalled capture from a slow one. Equal readings mean nothing arrived.
    let first = parser.capture_progress();
    let second = parser.capture_progress();
    assert_eq!(first, second, "progress must not move when no bytes arrive");
    assert!(first > 0, "progress must have advanced while bytes were arriving");

    // Feeding more must move it, or a host would cancel live transfers.
    parser.advance(b"BBBB");
    assert!(
        parser.capture_progress() > second,
        "progress must advance when bytes arrive, or a slow transfer reads as stalled"
    );

    // Having judged it stalled, the host cancels and gets the allocation back.
    let released = parser.cancel_capture();
    assert!(released > 0, "cancelling a live capture must release its staging");
    assert_eq!(parser.live_capture_count(), 0, "the capture must be gone");
    assert_eq!(parser.retained_amount().bytes, 0, "and its bytes with it");

    // Cancelling again is harmless — a host polling on a timer will do this.
    assert_eq!(parser.cancel_capture(), 0, "cancelling with nothing in flight must be a no-op");

    // The parser must still work afterwards; cancel resets state, not the session.
    parser.advance(b"hello");
    assert!(row_text(&parser, 0).starts_with("hello"), "the parser must still print after cancel");
}

/// Cancelling must not dispatch the partial payload.
///
/// A fragment of an image decodes to nothing useful, so surfacing it would
/// trade memory for a broken picture rather than no picture.
#[test]
fn cancelling_a_capture_emits_no_media_event() {
    let _serialised = POOLS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut parser = Parser::new(Grid::new(80, 24));
    parser.advance(b"\x1b_Gf=100;partial-payload-with-no-terminator");
    assert_eq!(parser.live_capture_count(), 1, "precondition: capture in flight");

    parser.cancel_capture();

    let events = parser.advance(b"x");
    assert!(
        !events.iter().any(|e| matches!(e, VtEvent::Media(_))),
        "a cancelled capture must not surface a truncated media event"
    );
}

/// Interleaved captures must hold the ceiling without help from the host.
///
/// Round-robin is the shape of real concurrent transfers. The bound asserted
/// here is the ceiling itself; it used to be
/// `MAX_PROCESS_CAPTURE_STAGING_BYTES.max(MIN_CAPTURE_STAGING_BYTES * PANES)`,
/// which grows with the pane count and so accommodated exactly the
/// unboundedness it looked like it was checking. A bound that widens to fit
/// the measurement is not a bound.
#[test]
fn interleaved_captures_hold_the_ceiling_without_a_reclaim_pass() {
    let _serialised = POOLS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    const PANES: usize = 20;
    const BLOCK: usize = 256 * 1024;

    let mut parsers: Vec<Parser> = (0..PANES).map(|_| Parser::new(Grid::new(80, 24))).collect();

    for parser in parsers.iter_mut() {
        parser.advance(b"\x1b_G");
    }

    let block = vec![b'A'; BLOCK];
    for _ in 0..(MAX_MEDIA_PAYLOAD_BYTES / BLOCK) {
        for parser in parsers.iter_mut() {
            parser.advance(&block);
        }
    }

    let total: usize = parsers.iter().map(|p| p.retained_amount().bytes).sum();
    assert!(
        total <= MAX_PROCESS_CAPTURE_STAGING_BYTES,
        "interleaved captures hold {} MiB against a ceiling of {} MiB",
        total / (1024 * 1024),
        MAX_PROCESS_CAPTURE_STAGING_BYTES / (1024 * 1024)
    );
}

/// A pane receiving a large image must get it whole.
///
/// The first operating principle is to give the user what they asked for. A
/// bound that quietly truncates an ordinary image would be a regression
/// dressed as a fix.
///
/// Sized against the floor rather than the per-capture maximum, because the
/// floor is what an admitted pane is guaranteed regardless of contention, and
/// guaranteeing it is the point.
#[test]
fn a_pane_receives_a_payload_up_to_the_guaranteed_floor_whole() {
    let _serialised = POOLS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut parser = Parser::new(Grid::new(80, 24));

    let payload_len = MIN_CAPTURE_STAGING_BYTES;
    let mut chunk = Vec::with_capacity(payload_len + 16);
    chunk.extend_from_slice(b"\x1b_Gf=100;");
    chunk.resize(payload_len, b'A');
    chunk.extend_from_slice(b"\x1b\\");

    let events = parser.advance(&chunk);
    let media = events
        .iter()
        .find_map(|e| match e {
            VtEvent::Media(m) => Some(m),
            _ => None,
        })
        .expect("a terminated kitty sequence must produce a media event");

    assert!(
        media.data.len() >= payload_len - 16,
        "a payload at the guaranteed floor must arrive whole: got {} of {payload_len} bytes",
        media.data.len()
    );
}

/// A capture cut by the per-capture maximum must not be dispatched.
///
/// A payload larger than a capture may hold arrives missing its tail, and the
/// tail is the rest of the image. Base64 protocols fail to decode it outright;
/// Sixel paints the fraction that arrived and reports it as a whole, shorter
/// picture. Neither is the image the user asked for, so neither is surfaced.
#[test]
fn a_payload_past_the_per_capture_maximum_is_not_dispatched() {
    let _serialised = POOLS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut parser = Parser::new(Grid::new(80, 24));

    let mut chunk = Vec::with_capacity(MAX_MEDIA_PAYLOAD_BYTES + 64);
    chunk.extend_from_slice(b"\x1b_Gf=100;");
    chunk.resize(MAX_MEDIA_PAYLOAD_BYTES + 32, b'A');
    chunk.extend_from_slice(b"\x1b\\");

    let events = parser.advance(&chunk);

    assert!(
        !events.iter().any(|e| matches!(e, VtEvent::Media(_))),
        "a payload past the per-capture maximum must not surface a cut-off picture"
    );
}

/// A lone pane is entitled to the full per-capture maximum, not merely the
/// floor.
///
/// Serialised, because it asserts the uncontended outcome and a concurrent
/// capture holding the growth pool would legitimately lower it. Kept separate
/// from the floor test above rather than merged so that a parallel run cannot
/// make the stronger claim silently vacuous.
#[test]
fn a_lone_capture_is_entitled_to_the_full_per_capture_maximum() {
    let _serialised = POOLS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());

    let mut capture = MediaCapture::new(MediaProtocol::Kitty, String::new());
    assert!(capture.admitted(), "a lone capture must be admitted");
    assert_eq!(
        LIVE_MEDIA_CAPTURES.load(Ordering::Relaxed),
        1,
        "precondition: this is the only capture in the process"
    );

    for _ in 0..MAX_MEDIA_PAYLOAD_BYTES {
        capture.append_byte(b'A');
    }

    assert_eq!(
        capture.data.len(),
        MAX_MEDIA_PAYLOAD_BYTES,
        "a lone capture must be able to grow to the full per-capture maximum"
    );
    assert!(!capture.truncated, "and must not be truncated on the way there");
}
