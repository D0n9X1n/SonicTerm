use super::*;
use crate::vt::vt_test_support::*;

/// Neovim's exact startup probe must learn the active curl without printing or changing it.
#[test]
fn decrqss_neovim_startup_probe_reports_curly_sgr() {
    let mut parser = Parser::new(Grid::new(8, 2));
    let (consumed, events, reply) = parser.advance_with_replies(b"\x1b[0m\x1b[4:3m\x1bP$qm\x1b\\");
    assert_eq!(consumed, 17);
    assert!(events.is_empty());
    assert_eq!(reply, b"\x1bP1$r0;4:3m\x1b\\");
    assert_eq!(parser.grid().cursor.col, 0);
    assert_eq!(parser.performer.underline_style, UnderlineStyle::Curly);
}

/// Neovim emits colon-form RGB underline color separately from its curly-style SGR.
#[test]
fn sgr_neovim_colon_underline_color_reaches_grid() {
    let mut parser = Parser::new(Grid::new(8, 2));
    parser.advance(b"\x1b[4:3m\x1b[58:2::255:0:0mW");
    let cell = &parser.grid().row(0)[0];
    assert_eq!(cell.ch, 'W');
    assert_eq!(cell.underline_style(), UnderlineStyle::Curly);
    assert_eq!(cell.underline_color(), Some(Color::Rgb(255, 0, 0)));
}

/// RGB and palette colon layouts apply independently to foreground, background, and underline.
#[test]
fn sgr_colon_colors_accept_supported_layouts() {
    for (suffix, expected) in [
        ("5:201", Color::Indexed(201)),
        ("2:255:0:17", Color::Rgb(255, 0, 17)),
        ("2::255:0:17", Color::Rgb(255, 0, 17)),
        ("2:0:255:0:17", Color::Rgb(255, 0, 17)),
    ] {
        let mut parser = Parser::new(Grid::new(8, 2));
        parser.advance(format!("\x1b[38:{suffix};48:{suffix};58:{suffix};4:3;1mX").as_bytes());
        let cell = &parser.grid().row(0)[0];
        assert_eq!(cell.fg, expected);
        assert_eq!(cell.bg, expected);
        assert_eq!(cell.underline_color(), Some(expected));
        assert_eq!(cell.underline_style(), UnderlineStyle::Curly);
        assert!(cell.flags.contains(CellFlags::BOLD));
    }
}

/// Rejected colon colors preserve prior semicolon colors and leave subsequent SGR parameters intact.
#[test]
fn sgr_invalid_colon_colors_preserve_state_without_swallowing_following_sgr() {
    for suffix in [
        "5:256",
        "5:1:2",
        "5",
        "2:1:2",
        "2:1:2:3:4:5",
        "2:1:2:3:4",
        "2:256:0:0",
        "2:0:256:0",
        "2:0:0:256",
        "2:0:256:0:0",
        "2:0:0:256:0",
        "2:0:0:0:256",
        "9:1:2:3",
    ] {
        for code in [38, 48, 58] {
            let mut parser = Parser::new(Grid::new(8, 2));
            parser.advance(b"\x1b[38;2;1;2;3;48;5;19;58;2;4;5;6m");
            parser.advance(format!("\x1b[{code}:{suffix};1;4:3mX").as_bytes());
            let cell = &parser.grid().row(0)[0];
            assert_eq!(cell.fg, Color::Rgb(1, 2, 3), "code={code} suffix={suffix}");
            assert_eq!(cell.bg, Color::Indexed(19), "code={code} suffix={suffix}");
            assert_eq!(
                cell.underline_color(),
                Some(Color::Rgb(4, 5, 6)),
                "code={code} suffix={suffix}"
            );
            assert!(cell.flags.contains(CellFlags::BOLD));
            assert_eq!(cell.underline_style(), UnderlineStyle::Curly);
        }
    }
}

/// Every active underline style and supported color survives an SGR query and replay.
#[test]
fn decrqss_roundtrips_all_supported_rendition_fields() {
    for style in 0..=5 {
        for color in ["", ";38;5;201;48;5;17;58;5;99", ";38;2;1;2;3;48;2;4;5;6;58;2;7;8;9"] {
            let mut parser = Parser::new(Grid::new(8, 2));
            parser.advance(format!("\x1b[1;2;3;4:{style};5;7;8;9{color}m").as_bytes());
            let flags = parser.performer.flags;
            let cursor = parser.grid().cursor;
            let (_, events, reply) = parser.advance_with_replies(b"\x1bP$qm\x1b\\");
            assert!(events.is_empty());
            assert!(reply.starts_with(b"\x1bP1$r0"));
            assert!(reply.ends_with(b"m\x1b\\"));
            assert_eq!(parser.performer.flags, flags);
            assert_eq!(parser.grid().cursor, cursor);
            assert_eq!(parser.retained_amount().bytes, 0);
            let sgr = &reply[5..reply.len() - 2];
            let mut replay = Parser::new(Grid::new(8, 2));
            replay.advance(b"\x1b[31;42;4:5;58;5;111m");
            replay.advance(&[b"\x1b[".as_slice(), sgr].concat());
            assert_eq!(replay.performer.fg, parser.performer.fg);
            assert_eq!(replay.performer.bg, parser.performer.bg);
            assert_eq!(replay.performer.flags, flags);
            assert_eq!(replay.performer.underline_style, parser.performer.underline_style);
            assert_eq!(replay.performer.underline_color, parser.performer.underline_color);
        }
    }
}

/// An unfinished ESC cannot answer a query; every chunk boundary preserves the final ST.
#[test]
fn decrqss_waits_for_complete_terminator_at_every_split() {
    for suffix in [b"\x1b\\".as_slice(), b"\x9c"] {
        let input = [b"\x1b[4:3m\x1bP$qm".as_slice(), suffix].concat();
        for split in 0..input.len() {
            let mut parser = Parser::new(Grid::new(8, 2));
            let (_, _, first) = parser.advance_with_replies(&input[..split]);
            assert!(first.is_empty(), "split={split}");
            let (_, _, last) = parser.advance_with_replies(&input[split..]);
            assert_eq!(last, b"\x1bP1$r0;4:3m\x1b\\");
        }
        let mut parser = Parser::new(Grid::new(8, 2));
        for (index, byte) in input.iter().enumerate() {
            let (_, _, reply) = parser.advance_with_replies(&[*byte]);
            assert_eq!(reply.is_empty(), index + 1 != input.len());
        }
    }
}

/// Invalid selectors and parameterized requests receive failure, never a fabricated SGR state.
#[test]
fn decrqss_rejects_unknown_selectors_and_headers() {
    for input in [
        b"\x1bP$q\x1b\\".as_slice(),
        b"\x1bP$qx\x1b\\",
        b"\x1bP$qmm\x1b\\",
        b"\x1bP$qm \x1b\\",
        b"\x1bP$qm\x7f\x1b\\",
        b"\x1bP$qm\xff\x1b\\",
        b"\x1bP1$qm\x1b\\",
        b"\x1bP0;0$qm\x1b\\",
        b"\x1bP0:0$qm\x1b\\",
    ] {
        let mut parser = Parser::new(Grid::new(8, 2));
        let (_, _, reply) = parser.advance_with_replies(input);
        assert_eq!(reply, b"\x1bP0$r\x1b\\", "input={input:?}");
    }
    for input in [b"\x1bP$+qm\x1b\\".as_slice(), b"\x1bP$qm\x1b[0m", b"\x1bP$$$$qm\x1b\\"] {
        let mut parser = Parser::new(Grid::new(8, 2));
        let (_, _, reply) = parser.advance_with_replies(input);
        assert!(reply.is_empty());
    }
    let mut parser = Parser::new(Grid::new(8, 2));
    let (_, _, reply) = parser.advance_with_replies(b"\x1bP0$qm\x1b\\");
    assert_eq!(reply, b"\x1bP1$r0m\x1b\\");
}

/// Cancellation and unrelated escapes must clear queries before any later terminator arrives.
#[test]
fn decrqss_cancellation_and_interruption_recover_without_reply() {
    let pool = CaptureStagingPool::new();
    for interruption in [
        b"\x18".as_slice(),
        b"\x1a",
        b"\x1b\x18",
        b"\x1b\x1a",
        b"\x1b\x1b\\",
        b"\x1b[0m",
        b"\x1b]2;title\x07",
        b"\x1b_unknown\x1b\\",
        b"\x1bc",
    ] {
        let mut parser = Parser::new_with_staging_pool(Grid::new(8, 2), None, pool.clone());
        parser.advance(b"\x1b[4:3m\x1bP$qm");
        for byte in interruption {
            let (_, _, reply) = parser.advance_with_replies(&[*byte]);
            assert!(reply.is_empty(), "interruption={interruption:?}");
        }
        let (_, _, reply) = parser.advance_with_replies(b"\x1b\\");
        assert!(reply.is_empty());
        assert!(parser.performer.decrqss.is_none());
        assert!(!parser.performer.decrqss_awaiting_backslash);
        let (_, _, reply) = parser.advance_with_replies(b"\x1b[0m\x1bP$qm\x1b\\");
        assert_eq!(reply, b"\x1bP1$r0m\x1b\\");
        parser.advance(b"X");
        assert_eq!(parser.grid().row(0)[0].ch, 'X');
    }
}

/// Oversized requests keep constant query storage and are discarded through ST without reply.
#[test]
fn decrqss_oversized_request_discards_and_recovers() {
    let mut parser = Parser::new(Grid::new(8, 2));
    parser.advance(b"\x1bP$q");
    parser.advance(&vec![b'm'; MAX_ESCAPE_SEQUENCE_BYTES + 1]);
    assert!(parser.performer.decrqss.is_none());
    assert_eq!(parser.retained_amount().bytes, 0);
    let (_, _, reply) = parser.advance_with_replies(b"\x1b\\");
    assert!(reply.is_empty());
    let (_, _, reply) = parser.advance_with_replies(b"\x1bP$qm\x1b\\");
    assert_eq!(reply, b"\x1bP1$r0m\x1b\\");
}

/// SGR reset and RIS report defaults rather than cached style or color from a previous query.
#[test]
fn decrqss_reset_reports_default_rendition() {
    for reset in [b"\x1b[0m".as_slice(), b"\x1bc"] {
        let mut parser = Parser::new(Grid::new(8, 2));
        parser.advance(b"\x1b[1;4:3;31;42;58;5;201m");
        parser.advance(reset);
        let (_, _, reply) = parser.advance_with_replies(b"\x1bP$qm\x1b\\");
        assert_eq!(reply, b"\x1bP1$r0m\x1b\\");
    }
}

/// Extended SGR keeps the explicit RGB color attached to a curly underline.
#[test]
fn sgr_curly_underline_preserves_explicit_rgb_color() {
    let mut parser = Parser::new(Grid::new(8, 1));

    parser.advance(b"\x1b[4:3;58;2;0;255;255mX");

    let cell = &parser.grid().row(0)[0];
    assert!(cell.flags.contains(CellFlags::UNDERLINE));
    assert_eq!(cell.underline_style(), UnderlineStyle::Curly);
    assert_eq!(cell.underline_color(), Some(Color::Rgb(0, 255, 255)));
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
fn captured_replies_resume_at_every_input_split_without_replaying_bytes() {
    // Yielding must preserve escape/UTF-8 state, ordered replies, and the printable suffix across every split.
    let input = "hello\x1b[3;7H\x1b[6n\x1b[5n世界!".as_bytes();
    for split in 0..=input.len() {
        let mut parser = Parser::new(Grid::new(80, 24));
        let mut replies = Vec::new();
        for chunk in [&input[..split], &input[split..]] {
            let mut offset = 0;
            while offset < chunk.len() {
                let (consumed, _, bytes) = parser.advance_with_replies(&chunk[offset..]);
                assert!(consumed > 0);
                offset += consumed;
                replies.extend(bytes);
            }
        }
        assert_eq!(replies, b"\x1b[3;7R\x1b[0n", "split={split}");
        assert_eq!(parser.grid().row(2)[6].ch, '世');
        assert_eq!(parser.grid().row(2)[10].ch, '!');
    }
}

#[test]
fn captured_palette_dispatch_exceeds_old_queue_without_loss() {
    // One capped OSC 4 dispatch may generate more replies than both old channels; stage it before yielding.
    let mut parser = Parser::new(Grid::new(80, 24));
    parser.set_theme_palette_color(0, 1, 2, 3);
    let pairs = "0;?;".repeat(1024);
    let input = format!("\x1b]4;{}\x07X", pairs.trim_end_matches(';'));
    let (consumed, _, replies) = parser.advance_with_replies(input.as_bytes());
    let expected = b"\x1b]4;0;rgb:0101/0202/0303\x07".repeat(1024);
    assert_eq!(replies, expected);
    assert!(replies.len() < 32 * 1024);
    assert_eq!(&input.as_bytes()[consumed..], b"X");
    let (consumed, _, replies) = parser.advance_with_replies(b"X");
    assert_eq!(consumed, 1);
    assert!(replies.is_empty());
    assert_eq!(parser.grid().row(0)[0].ch, 'X');
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
