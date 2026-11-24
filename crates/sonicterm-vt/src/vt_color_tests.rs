//! OSC 4, 10, 11, 12, 104 and 110-112 as a program sends them: sets, resets and queries, the
//! request order and valid-prefix rules, terminators, and whole-message capture.

use super::*;
use sonicterm_types::DynamicColor;

/// Feed `query` to `parser` as the VT worker does, one reply-bounded prefix at a time, and return
/// the reply bytes.
pub(super) fn reply_to(parser: &mut Parser, query: &[u8]) -> Vec<u8> {
    let mut reply = Vec::new();
    let mut remaining = query;
    while !remaining.is_empty() {
        let (consumed, _, replies) = parser.advance_with_replies(remaining);
        assert!(consumed > 0, "the parser makes progress on {query:?}");
        reply.extend(replies);
        remaining = &remaining[consumed..];
    }
    reply
}

/// A parser seeded with a known theme: palette entry `n` is `(n, n, n)`, foreground
/// `(200, 200, 200)`, background `(10, 10, 10)` and cursor `(250, 0, 0)`.
pub(super) fn themed_parser() -> Parser {
    let mut parser = Parser::new(Grid::new(8, 2));
    for index in 0..16u8 {
        parser.set_theme_palette_color(index, index, index, index);
    }
    parser.set_theme_fg(200, 200, 200);
    parser.set_theme_bg(10, 10, 10);
    parser.set_theme_cursor(250, 0, 0);
    let _ = parser.finish_theme_update();
    parser
}

/// The program's override of `target`, read through the public accessor.
pub(super) fn dynamic_override(parser: &Parser, target: DynamicColor) -> Option<(u8, u8, u8)> {
    parser.color_overrides().and_then(|overrides| overrides.dynamic(target))
}

/// The program's override of palette entry `index`, read through the public accessor.
pub(super) fn palette_override(parser: &Parser, index: u8) -> Option<(u8, u8, u8)> {
    parser.color_overrides().and_then(|overrides| overrides.palette(index))
}

/// A palette set in every spec form is answered by the next query with that color, as a 16-bit
/// reply ending in the request's own terminator.
#[test]
fn palette_set_then_query_answers_every_spec_form() {
    for spec in [
        "rgb:f/8/0",
        "rgb:ff/88/00",
        "rgb:fff/888/000",
        "rgb:ffff/8888/0000",
        "#f80",
        "#ff8800",
        "#fff888000",
        "#ffff88880000",
        "ff8800",
        "f80",
    ] {
        let mut parser = themed_parser();
        let input = format!("\x1b]4;3;{spec}\x07\x1b]4;3;?\x07");
        assert_eq!(reply_to(&mut parser, input.as_bytes()), b"\x1b]4;3;rgb:ffff/8888/0000\x07");
        let input = format!("\x1b]4;200;{spec}\x1b\\\x1b]4;200;?\x1b\\");
        assert_eq!(
            reply_to(&mut parser, input.as_bytes()),
            b"\x1b]4;200;rgb:ffff/8888/0000\x1b\\",
            "{spec}"
        );
    }
}

/// OSC 10, 11 and 12 set the foreground, background and cursor, read back by their queries; an
/// X11 name and an `rgbi:` intensity are accepted like any other form.
#[test]
fn dynamic_colors_set_then_query() {
    let mut parser = themed_parser();
    let reply = reply_to(
        &mut parser,
        b"\x1b]10;red\x07\x1b]11;rgbi:0/0/1\x07\x1b]12;#0f0\x07\x1b]10;?\x07\x1b]11;?\x07\x1b]12;?\x07",
    );
    assert_eq!(
        reply,
        b"\x1b]10;rgb:ffff/0000/0000\x07\x1b]11;rgb:0000/0000/ffff\x07\x1b]12;rgb:0000/ffff/0000\x07"
    );
    assert_eq!(dynamic_override(&parser, DynamicColor::Foreground), Some((255, 0, 0)));
    assert_eq!(dynamic_override(&parser, DynamicColor::Background), Some((0, 0, 255)));
    assert_eq!(dynamic_override(&parser, DynamicColor::Cursor), Some((0, 255, 0)));
}

/// Requests in one OSC 4 apply in order, so a query after a set in the same message answers the
/// new color, and a pair with an unreadable spec is skipped while the pairs after it still apply.
#[test]
fn osc4_applies_in_order_and_skips_an_invalid_pair() {
    let mut parser = themed_parser();
    assert_eq!(
        reply_to(&mut parser, b"\x1b]4;1;#010203;1;?;2;nope;3;#ffffff;1;?\x07"),
        b"\x1b]4;1;rgb:0101/0202/0303\x07\x1b]4;1;rgb:0101/0202/0303\x07"
    );
    assert_eq!(palette_override(&parser, 1), Some((1, 2, 3)));
    assert_eq!(palette_override(&parser, 2), None, "the pair with the bad spec is skipped");
    assert_eq!(palette_override(&parser, 3), Some((255, 255, 255)), "a later pair still applies");
}

/// An OSC 4 pair in the middle whose index is not a palette entry (a special color 256..=260,
/// a value past 260, or anything but digits) is skipped and the next pair still applies.
#[test]
fn osc4_skips_a_pair_with_a_bad_index() {
    let mut parser = themed_parser();
    for skipped in ["256", "260", "261", "999", "+5", "0_5", " 5", "x"] {
        let _ = reply_to(&mut parser, b"\x1b]104;3\x07");
        let input = format!("\x1b]4;{skipped};#fff;3;#fff\x07");
        let _ = reply_to(&mut parser, input.as_bytes());
        assert_eq!(palette_override(&parser, 3), Some((255, 255, 255)), "{skipped:?} is skipped");
        assert_eq!(palette_override(&parser, 5), None, "{skipped:?} sets no entry");
    }
    let _ = reply_to(&mut parser, b"\x1b]4;256;nope;2;#fff\x07");
    assert_eq!(palette_override(&parser, 2), Some((255, 255, 255)));
    assert_eq!(reply_to(&mut parser, b"\x1b]4;256;?\x07"), b"", "special colors are not reported");
}

/// A lone index after the complete OSC 4 pairs is ignored, while every complete pair before it
/// applies, as Ghostty and WezTerm keep the prefix.
#[test]
fn osc4_ignores_an_odd_trailing_index() {
    let mut parser = themed_parser();
    assert_eq!(reply_to(&mut parser, b"\x1b]4;6;#060606;8;#080808;7\x07"), b"");
    assert_eq!(palette_override(&parser, 6), Some((6, 6, 6)));
    assert_eq!(palette_override(&parser, 8), Some((8, 8, 8)));
    assert_eq!(palette_override(&parser, 7), None, "the trailing index does nothing");
}

/// One OSC 4 longer than vte's sixteen parameters is applied whole: the collector owns the
/// message, so no pair past the parameter cap is lost.
#[test]
fn a_long_osc4_set_applies_every_pair() {
    let mut parser = themed_parser();
    let mut input = String::from("\x1b]4");
    for index in 0..64u8 {
        input.push_str(&format!(";{index};rgb:{index:02x}/00/00"));
    }
    input.push('\x07');
    let _ = reply_to(&mut parser, input.as_bytes());
    for index in 0..64u8 {
        assert_eq!(palette_override(&parser, index), Some((index, 0, 0)), "index {index}");
    }
}

/// OSC 104 resets the listed entries, skipping an invalid index; a bare request or a list of
/// only blanks resets every overridden entry, while a list whose entries are all invalid, or a
/// special-color index, changes nothing.
#[test]
fn osc104_resets_listed_entries_or_the_whole_palette() {
    let mut parser = themed_parser();
    let set_three = b"\x1b]4;1;#111111;2;#222222;3;#333333\x07";
    let _ = reply_to(&mut parser, set_three);
    let _ = reply_to(&mut parser, b"\x1b]104;1\x07");
    assert_eq!(palette_override(&parser, 1), None);
    assert_eq!(palette_override(&parser, 2), Some((0x22, 0x22, 0x22)));
    let _ = reply_to(&mut parser, b"\x1b]104;x;3;;999\x07");
    assert_eq!(palette_override(&parser, 3), None, "the invalid index is skipped, 3 resets");
    assert_eq!(palette_override(&parser, 2), Some((0x22, 0x22, 0x22)));
    let _ = reply_to(&mut parser, b"\x1b]104;256\x07");
    assert_eq!(palette_override(&parser, 2), Some((0x22, 0x22, 0x22)), "256 resets nothing");
    for unchanged in [&b"\x1b]104;x\x07"[..], b"\x1b]104;;x;999\x07", b"\x1b]104;+1\x1b\\"] {
        let _ = reply_to(&mut parser, unchanged);
        assert_eq!(palette_override(&parser, 2), Some((0x22, 0x22, 0x22)), "{unchanged:?}");
    }
    for reset_all in [
        &b"\x1b]104\x07"[..],
        b"\x1b]104\x1b\\",
        b"\x1b]104;\x07",
        b"\x1b]104;;\x07",
        b"\x1b]104; \x07",
    ] {
        let _ = reply_to(&mut parser, set_three);
        let _ = reply_to(&mut parser, reset_all);
        for index in 1..=3 {
            assert_eq!(palette_override(&parser, index), None, "{reset_all:?} resets {index}");
        }
    }
    assert_eq!(reply_to(&mut parser, b"\x1b]4;2;?\x07"), b"\x1b]4;2;rgb:0202/0202/0202\x07");
}

/// OSC 110, 111 and 112 reset their own color with no argument or any argument but `?` (empty
/// separators or values), and never the next color: `110;x;y` leaves the background set.
#[test]
fn dynamic_resets_apply_to_any_argument_but_a_query() {
    for (code, target) in [
        (110, DynamicColor::Foreground),
        (111, DynamicColor::Background),
        (112, DynamicColor::Cursor),
    ] {
        let set = format!("\x1b]{};#123456\x07", code - 100);
        for reset in [
            format!("\x1b]{code}\x07"),
            format!("\x1b]{code};\x1b\\"),
            format!("\x1b]{code};;\x07"),
            format!("\x1b]{code};x\x07"),
            format!("\x1b]{code};#000000;y\x07"),
        ] {
            let mut parser = themed_parser();
            let _ = reply_to(&mut parser, set.as_bytes());
            assert_eq!(reply_to(&mut parser, reset.as_bytes()), b"", "{reset:?} answers nothing");
            assert_eq!(dynamic_override(&parser, target), None, "{reset:?}");
        }
    }
    let mut parser = themed_parser();
    let _ = reply_to(&mut parser, b"\x1b]10;#010101;#020202\x07");
    let _ = reply_to(&mut parser, b"\x1b]110;x;y\x07");
    assert_eq!(dynamic_override(&parser, DynamicColor::Foreground), None);
    assert_eq!(dynamic_override(&parser, DynamicColor::Background), Some((2, 2, 2)));
}

/// A `?` argument to OSC 110, 111 or 112 asks for the named color instead of resetting it, as
/// kitty reads it, and the reply carries the request's own number.
#[test]
fn a_dynamic_reset_with_a_query_answers_instead() {
    let mut parser = themed_parser();
    let _ = reply_to(&mut parser, b"\x1b]11;#121314\x07");
    assert_eq!(reply_to(&mut parser, b"\x1b]111;?\x1b\\"), b"\x1b]111;rgb:1212/1313/1414\x1b\\");
    assert_eq!(dynamic_override(&parser, DynamicColor::Background), Some((0x12, 0x13, 0x14)));
    assert_eq!(reply_to(&mut parser, b"\x1b]112;?\x07"), b"\x1b]112;rgb:fafa/0000/0000\x07");
    assert_eq!(reply_to(&mut parser, b"\x1b]113;?\x07"), b"", "113 keeps no color to report");
}

/// OSC 13-16, 18, 113-116 and 118 name colors this terminal does not keep: they are consumed whole,
/// answer nothing, change nothing and print nothing, however many values they carry.
#[test]
fn unkept_dynamic_colors_are_consumed_silently() {
    let mut parser = themed_parser();
    let many_queries = format!("\x1b]13{}\x07", ";?".repeat(24));
    for message in [
        many_queries.as_bytes(),
        b"\x1b]13;?\x07",
        b"\x1b]14;?\x07",
        b"\x1b]15;#ffffff\x07",
        b"\x1b]16;?;?\x07",
        b"\x1b]18;red\x07",
        b"\x1b]113\x07",
        b"\x1b]114;x\x07",
        b"\x1b]115;?\x07",
        b"\x1b]116\x07",
        b"\x1b]118;?\x07",
    ] {
        assert_eq!(reply_to(&mut parser, message), b"", "{message:?}");
        assert!(parser.color_overrides().is_none(), "{message:?}");
    }
    parser.advance(b"Z");
    assert_eq!(parser.grid().row(0)[0].ch, 'Z', "nothing was printed before the Z");
}

/// Each further OSC 10 parameter targets the next dynamic color, so `10;?;?` answers the
/// foreground and the background and `10;#fff;?` sets the foreground then reads the background.
#[test]
fn chained_dynamic_requests_walk_the_dynamic_colors() {
    let mut parser = themed_parser();
    assert_eq!(
        reply_to(&mut parser, b"\x1b]10;?;?\x1b\\"),
        b"\x1b]10;rgb:c8c8/c8c8/c8c8\x1b\\\x1b]11;rgb:0a0a/0a0a/0a0a\x1b\\"
    );
    assert_eq!(reply_to(&mut parser, b"\x1b]10;#fff;?\x07"), b"\x1b]11;rgb:0a0a/0a0a/0a0a\x07");
    assert_eq!(dynamic_override(&parser, DynamicColor::Foreground), Some((255, 255, 255)));
    assert_eq!(
        reply_to(&mut parser, b"\x1b]12;?;?;?\x07"),
        b"\x1b]12;rgb:fafa/0000/0000\x07",
        "colors past the cursor are not reported"
    );
    assert_eq!(
        reply_to(&mut parser, b"\x1b]10;;?\x07"),
        b"\x1b]11;rgb:0a0a/0a0a/0a0a\x07",
        "an empty value advances to the next color"
    );
    let _ = reply_to(&mut parser, b"\x1b]110\x07\x1b]10;;#050505\x07");
    assert_eq!(
        dynamic_override(&parser, DynamicColor::Foreground),
        None,
        "the empty value sets nothing"
    );
    assert_eq!(dynamic_override(&parser, DynamicColor::Background), Some((5, 5, 5)));
    let _ = reply_to(&mut parser, b"\x1b]111\x07");
    let ten_queries = format!("\x1b]11{}\x07", ";?".repeat(10));
    assert_eq!(
        reply_to(&mut parser, ten_queries.as_bytes()),
        b"\x1b]11;rgb:0a0a/0a0a/0a0a\x07\x1b]12;rgb:fafa/0000/0000\x07",
        "the walk passes 13-19 silently and stops after 19"
    );
    let _ = reply_to(&mut parser, b"\x1b]11;nope;#000\x07");
    assert_eq!(
        dynamic_override(&parser, DynamicColor::Background),
        None,
        "the bad value is skipped"
    );
    assert_eq!(
        dynamic_override(&parser, DynamicColor::Cursor),
        Some((0, 0, 0)),
        "the walk goes on past a bad value"
    );
}

/// A cursor with no override and no theme cursor answers the effective foreground, override
/// included.
#[test]
fn an_unset_cursor_answers_the_effective_foreground() {
    let mut parser = Parser::new(Grid::new(8, 2));
    parser.set_theme_fg(200, 200, 200);
    assert_eq!(reply_to(&mut parser, b"\x1b]12;?\x07"), b"\x1b]12;rgb:c8c8/c8c8/c8c8\x07");
    let _ = reply_to(&mut parser, b"\x1b]10;#010203\x07");
    assert_eq!(reply_to(&mut parser, b"\x1b]12;?\x07"), b"\x1b]12;rgb:0101/0202/0303\x07");
}

/// An override answers even for a palette slot the theme never seeded, while an unseeded,
/// unoverridden slot stays silent.
#[test]
fn an_override_answers_an_unseeded_slot() {
    let mut parser = Parser::new(Grid::new(8, 2));
    assert_eq!(reply_to(&mut parser, b"\x1b]4;5;?\x07"), b"");
    assert_eq!(
        reply_to(&mut parser, b"\x1b]4;5;#0a0b0c;5;?\x07"),
        b"\x1b]4;5;rgb:0a0a/0b0b/0c0c\x07"
    );
}

/// A message past the 4 KiB cap, one aborted by CAN or SUB, and one cut by an ESC that does not
/// start ST each apply nothing and reply nothing: no truncated set is ever applied.
#[test]
fn over_cap_and_aborted_messages_apply_nothing() {
    let mut over_cap = String::from("\x1b]4;1;#ffffff;1;?");
    over_cap.push_str(&";".repeat(4096));
    over_cap.push('\x07');
    for message in [
        over_cap.into_bytes(),
        b"\x1b]4;1;#ffffff;1;?\x18".to_vec(),
        b"\x1b]10;#ffffff;?\x1a".to_vec(),
        b"\x1b]104\x18".to_vec(),
        b"\x1b]4;1;#ffffff;1;?\x1b[m".to_vec(),
    ] {
        let mut parser = themed_parser();
        let _ = reply_to(&mut parser, b"\x1b]4;7;#070707\x07");
        assert_eq!(reply_to(&mut parser, &message), b"", "{message:?}");
        assert_eq!(palette_override(&parser, 1), None, "{message:?}");
        assert_eq!(dynamic_override(&parser, DynamicColor::Foreground), None, "{message:?}");
        assert_eq!(palette_override(&parser, 7), Some((7, 7, 7)), "{message:?} reset nothing");
        parser.advance(b"Z");
        assert_eq!(parser.grid().row(0)[0].ch, 'Z', "{message:?} leaves the parser in ground");
    }
}

/// A query gets exactly one reply, ended by ST or BEL as the request was: the collector owns the
/// message, so vte never dispatches it a second time.
#[test]
fn each_query_is_answered_once_with_its_terminator() {
    let (tx, rx) = crossbeam_channel::unbounded();
    let mut parser = Parser::new_with_reply(Grid::new(8, 2), tx);
    parser.set_theme_bg(1, 2, 3);
    parser.advance(b"\x1b]11;?\x1b\\\x1b]11;?\x07");
    assert_eq!(rx.try_recv().unwrap(), b"\x1b]11;rgb:0101/0202/0303\x1b\\".to_vec());
    assert_eq!(rx.try_recv().unwrap(), b"\x1b]11;rgb:0101/0202/0303\x07".to_vec());
    assert!(rx.try_recv().is_err(), "no duplicate reply");
}

/// Only the exact OSC numbers are color requests: a zero-padded `04`, which Ghostty does not read
/// as OSC 4, neither sets nor answers.
#[test]
fn only_exact_osc_numbers_are_color_requests() {
    let mut parser = themed_parser();
    assert_eq!(reply_to(&mut parser, b"\x1b]04;1;#ffffff;1;?\x07"), b"");
    assert_eq!(reply_to(&mut parser, b"\x1b]010;?\x07"), b"");
    assert_eq!(palette_override(&parser, 1), None);
}

/// A fresh parser reports no overrides, so a pane that never set a color takes the theme-only
/// render path; it reports them once set, and none again once every one is reset.
#[test]
fn overrides_are_reported_only_while_a_program_holds_one() {
    let mut parser = themed_parser();
    assert!(parser.color_overrides().is_none());
    let _ = reply_to(&mut parser, b"\x1b]11;#000\x07\x1b]4;9;#fff\x07");
    let (grid, overrides) = parser.grid_and_color_overrides();
    assert_eq!(grid.cols, 8);
    assert_eq!(overrides.and_then(|overrides| overrides.palette(9)), Some((255, 255, 255)));
    let _ = reply_to(&mut parser, b"\x1b]111\x07\x1b]104;9\x07");
    assert!(parser.color_overrides().is_none());
}
