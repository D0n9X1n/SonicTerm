//! Selection and cursor-text colors: OSC 17 and 19 sets and queries, the 117 and 119 resets, the
//! OSC 21 `selection_*` and `cursor_text` keys sharing their state, and a pywal color scheme.

use super::vt_color_tests::{dynamic_override, palette_override, reply_to, themed_parser};
use super::*;
use sonicterm_types::DynamicColor;

/// A themed parser that also knows the theme selection background `(30, 31, 32)`, selection
/// foreground `(40, 41, 42)` and cursor text `(50, 51, 52)`.
fn selection_parser() -> Parser {
    let mut parser = themed_parser();
    parser.set_theme_selection_bg(30, 31, 32);
    parser.set_theme_selection_fg(40, 41, 42);
    parser.set_theme_cursor_text(50, 51, 52);
    parser
}

/// OSC 17 and 19 answer the theme selection colors as 16-bit replies with the request's
/// terminator, set the selection background and foreground, and answer the new color after a set.
#[test]
fn osc17_and_osc19_set_and_query_the_selection_colors() {
    let mut parser = selection_parser();
    assert_eq!(
        reply_to(&mut parser, b"\x1b]17;?\x07\x1b]19;?\x1b\\"),
        b"\x1b]17;rgb:1e1e/1f1f/2020\x07\x1b]19;rgb:2828/2929/2a2a\x1b\\"
    );
    let _ = reply_to(&mut parser, b"\x1b]17;#123456\x07\x1b]19;red\x07");
    assert_eq!(
        dynamic_override(&parser, DynamicColor::SelectionBackground),
        Some((0x12, 0x34, 0x56))
    );
    assert_eq!(dynamic_override(&parser, DynamicColor::SelectionForeground), Some((255, 0, 0)));
    assert_eq!(
        reply_to(&mut parser, b"\x1b]17;?\x07\x1b]19;?\x07"),
        b"\x1b]17;rgb:1212/3434/5656\x07\x1b]19;rgb:ffff/0000/0000\x07"
    );
    let _ = reply_to(&mut parser, b"\x1b]17;nope\x07");
    assert_eq!(
        dynamic_override(&parser, DynamicColor::SelectionBackground),
        Some((0x12, 0x34, 0x56)),
        "an invalid spec is skipped"
    );
}

/// OSC 117 and 119 reset only their own color with no argument, an empty one or any first argument
/// but `?`, and answer it under their own number only for a first-field `?`.
#[test]
fn osc117_and_osc119_reset_or_answer_their_own_color() {
    for (code, target, theme) in [
        (117, DynamicColor::SelectionBackground, "1e1e/1f1f/2020"),
        (119, DynamicColor::SelectionForeground, "2828/2929/2a2a"),
    ] {
        for reset in [
            format!("\x1b]{code}\x07"),
            format!("\x1b]{code}\x1b\\"),
            format!("\x1b]{code};\x07"),
            format!("\x1b]{code};x\x07"),
        ] {
            let mut parser = selection_parser();
            let _ = reply_to(&mut parser, b"\x1b]17;#010101\x07\x1b]19;#020202\x07");
            assert_eq!(reply_to(&mut parser, reset.as_bytes()), b"", "{reset:?}");
            assert_eq!(dynamic_override(&parser, target), None, "{reset:?}");
            let other = if code == 117 {
                DynamicColor::SelectionForeground
            } else {
                // When: the reset is 119, the selection background is the color left alone.
                DynamicColor::SelectionBackground
            };
            assert!(dynamic_override(&parser, other).is_some(), "{reset:?} touches only {code}");
        }
        let mut parser = selection_parser();
        let query = format!("\x1b]{code};?\x07");
        assert_eq!(
            reply_to(&mut parser, query.as_bytes()),
            format!("\x1b]{code};rgb:{theme}\x07").into_bytes()
        );
        // A `?` after the first field is an ignored parameter: the color resets, nothing answers.
        for mixed in [format!("\x1b]{code};x;?\x07"), format!("\x1b]{code};;?\x07")] {
            let mut parser = selection_parser();
            let _ = reply_to(&mut parser, b"\x1b]17;#010101\x07\x1b]19;#020202\x07");
            assert_eq!(reply_to(&mut parser, mixed.as_bytes()), b"", "{mixed:?}");
            assert_eq!(dynamic_override(&parser, target), None, "{mixed:?} resets");
        }
    }
}

/// A chained OSC 10 query walks 10 through 19 and answers only the colors this terminal keeps: 10,
/// 11, 12, 17 and 19, in order; 13-16 and 18 stay silent and their resets do nothing.
#[test]
fn a_chained_query_answers_10_11_12_17_and_19_only() {
    let mut parser = selection_parser();
    let chain = format!("\x1b]10{}\x07", ";?".repeat(10));
    assert_eq!(
        reply_to(&mut parser, chain.as_bytes()),
        b"\x1b]10;rgb:c8c8/c8c8/c8c8\x07\x1b]11;rgb:0a0a/0a0a/0a0a\x07\x1b]12;rgb:fafa/0000/0000\x07\x1b]17;rgb:1e1e/1f1f/2020\x07\x1b]19;rgb:2828/2929/2a2a\x07"
    );
    let _ = reply_to(&mut parser, b"\x1b]17;#111111;#222222;#333333\x07");
    assert_eq!(
        dynamic_override(&parser, DynamicColor::SelectionBackground),
        Some((0x11, 0x11, 0x11))
    );
    assert_eq!(
        dynamic_override(&parser, DynamicColor::SelectionForeground),
        Some((0x33, 0x33, 0x33)),
        "18 is skipped"
    );
    for inert in [&b"\x1b]18;?\x07"[..], b"\x1b]118;?\x07", b"\x1b]16;?\x07", b"\x1b]113\x07"] {
        assert_eq!(reply_to(&mut parser, inert), b"", "{inert:?}");
    }
}

/// OSC 17/19/117/119 and the OSC 21 keys read and write one state: a set through either is
/// answered by the other, and a reset through either returns the theme value to both.
#[test]
fn osc21_selection_keys_share_state_with_osc17_and_osc19() {
    let mut parser = selection_parser();
    let _ = reply_to(&mut parser, b"\x1b]21;selection_background=#123456\x07");
    assert_eq!(reply_to(&mut parser, b"\x1b]17;?\x07"), b"\x1b]17;rgb:1212/3434/5656\x07");
    let _ = reply_to(&mut parser, b"\x1b]19;#abcdef\x07");
    assert_eq!(
        reply_to(&mut parser, b"\x1b]21;selection_foreground=?\x07"),
        b"\x1b]21;selection_foreground=rgb:ab/cd/ef\x07"
    );
    let _ = reply_to(&mut parser, b"\x1b]119\x07");
    assert_eq!(
        reply_to(&mut parser, b"\x1b]21;selection_foreground=?\x07"),
        b"\x1b]21;selection_foreground=rgb:28/29/2a\x07"
    );
    for reset in ["selection_background=", "selection_background"] {
        let _ = reply_to(&mut parser, b"\x1b]17;#010203\x07");
        let _ = reply_to(&mut parser, format!("\x1b]21;{reset}\x07").as_bytes());
        assert_eq!(dynamic_override(&parser, DynamicColor::SelectionBackground), None, "{reset}");
    }
}

/// `cursor_text` is reachable only through OSC 21: a set, a query, and both reset forms; with no
/// theme seed and no override it answers empty.
#[test]
fn osc21_cursor_text_sets_answers_and_resets() {
    let mut parser = selection_parser();
    assert_eq!(
        reply_to(&mut parser, b"\x1b]21;cursor_text=#123456@0.5;cursor_text=?\x07"),
        b"\x1b]21;cursor_text=rgb:12/34/56\x07"
    );
    assert_eq!(dynamic_override(&parser, DynamicColor::CursorText), Some((0x12, 0x34, 0x56)));
    for reset in ["cursor_text=", "cursor_text"] {
        let _ = reply_to(&mut parser, b"\x1b]21;cursor_text=#123456\x07");
        assert_eq!(
            reply_to(&mut parser, format!("\x1b]21;{reset};cursor_text=?\x07").as_bytes()),
            b"\x1b]21;cursor_text=rgb:32/33/34\x07",
            "{reset}"
        );
        assert_eq!(dynamic_override(&parser, DynamicColor::CursorText), None, "{reset}");
    }
    let mut unseeded = Parser::new(Grid::new(8, 2));
    assert_eq!(reply_to(&mut unseeded, b"\x1b]21;cursor_text=?\x07"), b"\x1b]21;cursor_text=\x07");
}

/// pywal's non-Darwin, default-alpha scheme applies palette 0-15, foreground, background, cursor,
/// selection background (17) and foreground (19) and palette 232; it ignores 13 and 708, skips
/// the out-of-range 256 and 257, and prints nothing.
#[test]
fn a_pywal_scheme_applies_its_selection_colors() {
    let mut parser = selection_parser();
    let mut scheme = String::new();
    for index in 0..16u8 {
        scheme.push_str(&format!("\x1b]4;{index};#{index:02x}0000\x1b\\"));
    }
    for (code, color) in [
        (10, "#a0a0a0"),
        (11, "#0b0b0b"),
        (12, "#c0c0c0"),
        (13, "#a0a0a0"),
        (17, "#a0a0a0"),
        (19, "#0b0b0b"),
    ] {
        scheme.push_str(&format!("\x1b]{code};{color}\x1b\\"));
    }
    scheme.push_str("\x1b]4;232;#0b0b0b\x1b\\\x1b]4;256;#a0a0a0\x1b\\\x1b]4;257;#0b0b0b\x1b\\");
    scheme.push_str("\x1b]708;#0b0b0b\x1b\\");
    assert_eq!(reply_to(&mut parser, scheme.as_bytes()), b"");
    for index in 0..16u8 {
        assert_eq!(palette_override(&parser, index), Some((index, 0, 0)), "palette {index}");
    }
    assert_eq!(palette_override(&parser, 232), Some((11, 11, 11)));
    for (target, rgb) in [
        (DynamicColor::Foreground, (0xa0, 0xa0, 0xa0)),
        (DynamicColor::Background, (11, 11, 11)),
        (DynamicColor::Cursor, (0xc0, 0xc0, 0xc0)),
        (DynamicColor::SelectionBackground, (0xa0, 0xa0, 0xa0)),
        (DynamicColor::SelectionForeground, (11, 11, 11)),
    ] {
        assert_eq!(dynamic_override(&parser, target), Some(rgb), "{target:?}");
    }
    parser.advance(b"Z");
    assert_eq!(parser.grid().row(0)[0].ch, 'Z', "the scheme printed nothing");
}

/// The selection and cursor-text overrides survive a theme reseed and RIS, and setting them sends
/// no mode 2031 report.
#[test]
fn selection_overrides_survive_reseed_and_ris_without_a_2031_report() {
    let mut parser = selection_parser();
    parser.advance(b"\x1b[?2031h");
    let replies = reply_to(
        &mut parser,
        b"\x1b]17;#010101\x07\x1b]19;#020202\x07\x1b]21;cursor_text=#030303\x07\x1b]117\x07\x1b]17;#010101\x07",
    );
    assert_eq!(replies, b"", "a program change reports nothing under mode 2031");
    parser.set_theme_selection_bg(99, 99, 99);
    parser.set_theme_selection_fg(98, 98, 98);
    parser.set_theme_cursor_text(97, 97, 97);
    let _ = parser.finish_theme_update();
    let _ = reply_to(&mut parser, b"\x1bc");
    assert_eq!(dynamic_override(&parser, DynamicColor::SelectionBackground), Some((1, 1, 1)));
    assert_eq!(dynamic_override(&parser, DynamicColor::SelectionForeground), Some((2, 2, 2)));
    assert_eq!(dynamic_override(&parser, DynamicColor::CursorText), Some((3, 3, 3)));
}
