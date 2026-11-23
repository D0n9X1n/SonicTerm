//! OSC 21, the kitty color protocol, as a program sends it: the key table in every form, request
//! order, unknown keys, normalization, the item budget, the input cap and the reply bound.

use super::super::vt_color_tests::{dynamic_override, palette_override, reply_to, themed_parser};
use super::*;
use sonicterm_types::DynamicColor;

/// The largest reply one pane reply-spool submission admits.
const SPOOL_SUBMISSION_BYTES: usize = 32 * 1024;

/// One OSC 21 reply carrying `fields` (`key=value` each) with `terminator`.
fn kitty_reply(fields: &[&str], terminator: &str) -> Vec<u8> {
    format!("\x1b]21;{}{terminator}", fields.join(";")).into_bytes()
}

/// A mixed packet applies and answers in request order, so a query after a set in the same packet
/// reads the new color, and a key named twice applies twice, the later one winning.
#[test]
fn a_mixed_packet_applies_and_answers_in_order() {
    let mut parser = themed_parser();
    let reply = reply_to(
        &mut parser,
        b"\x1b]21;foreground=?;foreground=#010203;foreground=?;1=#0a0b0c;1=?;1;1=?;5=#050505;5=#060606\x07",
    );
    assert_eq!(
        reply,
        kitty_reply(
            &[
                "foreground=rgb:c8/c8/c8",
                "foreground=rgb:01/02/03",
                "1=rgb:0a/0b/0c",
                "1=rgb:01/01/01",
            ],
            "\x07"
        )
    );
    assert_eq!(dynamic_override(&parser, DynamicColor::Foreground), Some((1, 2, 3)));
    assert_eq!(palette_override(&parser, 1), None, "the bare key reset entry 1");
    assert_eq!(palette_override(&parser, 5), Some((6, 6, 6)), "the later set wins");
}

/// Palette keys 0-255 in all four forms: `?` answers the effective entry (theme seed for 0-15,
/// the xterm table above), a spec sets it, and an empty value or a bare key resets it. The reply
/// echoes the key as sent, leading zeros included.
#[test]
fn palette_keys_query_set_and_reset() {
    let mut parser = themed_parser();
    assert_eq!(
        reply_to(&mut parser, b"\x1b]21;3=?;200=?;007=?\x1b\\"),
        kitty_reply(&["3=rgb:03/03/03", "200=rgb:ff/00/d7", "007=rgb:07/07/07"], "\x1b\\")
    );
    for (set, reset) in [("3=#abcdef", "3="), ("3=#abcdef", "3"), ("200=red", "200=")] {
        let key: u8 = set.split('=').next().unwrap().parse().unwrap();
        let _ = reply_to(&mut parser, format!("\x1b]21;{set}\x07").as_bytes());
        assert!(palette_override(&parser, key).is_some(), "{set} sets entry {key}");
        let _ = reply_to(&mut parser, format!("\x1b]21;{reset}\x07").as_bytes());
        assert_eq!(palette_override(&parser, key), None, "{reset} resets entry {key}");
    }
    let _ = reply_to(&mut parser, b"\x1b]21;200 = #102030 \x07");
    assert_eq!(palette_override(&parser, 200), None, "a key is not trimmed: `200 ` is unknown");
    let _ = reply_to(&mut parser, b"\x1b]21;200= #102030 \x07");
    assert_eq!(palette_override(&parser, 200), Some((0x10, 0x20, 0x30)), "a value is trimmed");
    let _ = reply_to(&mut parser, b"\x1b]21;200=   \x07");
    assert_eq!(palette_override(&parser, 200), None, "a blank value is empty and resets");
}

/// `foreground`, `background` and `cursor` in all four forms: `?` answers the effective color, a
/// spec sets it, and an empty value or a bare key resets it to the theme.
#[test]
fn special_keys_query_set_and_reset() {
    for (key, target, theme) in [
        ("foreground", DynamicColor::Foreground, "c8/c8/c8"),
        ("background", DynamicColor::Background, "0a/0a/0a"),
        ("cursor", DynamicColor::Cursor, "fa/00/00"),
    ] {
        for reset in [format!("{key}="), key.to_string()] {
            let mut parser = themed_parser();
            let packet = format!("\x1b]21;{key}=#123456;{key}=?;{reset};{key}=?\x07");
            assert_eq!(
                reply_to(&mut parser, packet.as_bytes()),
                kitty_reply(
                    &[&format!("{key}=rgb:12/34/56"), &format!("{key}=rgb:{theme}")],
                    "\x07"
                ),
                "{reset}"
            );
            assert_eq!(dynamic_override(&parser, target), None, "{reset} reset {key}");
        }
    }
}

/// `cursor=?` always answers what OSC 12 reports: the override, else the theme cursor, else the
/// effective foreground; with none of them it answers empty.
#[test]
fn cursor_query_matches_the_osc12_answer() {
    let mut parser = themed_parser();
    assert_eq!(reply_to(&mut parser, b"\x1b]12;?\x07"), b"\x1b]12;rgb:fafa/0000/0000\x07");
    assert_eq!(
        reply_to(&mut parser, b"\x1b]21;cursor=?\x07"),
        kitty_reply(&["cursor=rgb:fa/00/00"], "\x07")
    );
    let _ = reply_to(&mut parser, b"\x1b]12;#0d0e0f\x07");
    assert_eq!(
        reply_to(&mut parser, b"\x1b]21;cursor=?\x07"),
        kitty_reply(&["cursor=rgb:0d/0e/0f"], "\x07")
    );
    let mut parser = Parser::new(Grid::new(8, 2));
    parser.set_theme_fg(200, 200, 200);
    assert_eq!(reply_to(&mut parser, b"\x1b]12;?\x07"), b"\x1b]12;rgb:c8c8/c8c8/c8c8\x07");
    assert_eq!(
        reply_to(&mut parser, b"\x1b]21;cursor=?\x07"),
        kitty_reply(&["cursor=rgb:c8/c8/c8"], "\x07")
    );
    let mut parser = Parser::new(Grid::new(8, 2));
    assert_eq!(
        reply_to(&mut parser, b"\x1b]21;cursor=?;4=?\x07"),
        kitty_reply(&["cursor=", "4="], "\x07")
    );
}

/// Keys this terminal recognizes but does not hold (`visual_bell`, `transparent_background_color1`
/// through `8`) answer a query with an empty value, and their sets and resets change nothing.
#[test]
fn unheld_keys_answer_empty_and_ignore_sets() {
    let mut parser = themed_parser();
    for key in [
        "visual_bell",
        "transparent_background_color3",
        "transparent_background_color1",
        "transparent_background_color8",
        "transparent_background_color01",
    ] {
        let packet = format!("\x1b]21;{key}=#ffffff@0.5;{key}=;{key};{key}=?\x07");
        assert_eq!(
            reply_to(&mut parser, packet.as_bytes()),
            kitty_reply(&[&format!("{key}=")], "\x07"),
            "{key}"
        );
        assert!(parser.color_overrides().is_none(), "{key} changes nothing");
    }
}

/// Every other key, out-of-range indices and Ghostty's `second_transparent_background` included,
/// answers `unknown=` with the key in unpadded base64, in order among the other answers, in every
/// form.
#[test]
fn unknown_keys_answer_their_base64_name_in_order() {
    let mut parser = themed_parser();
    assert_eq!(
        reply_to(&mut parser, b"\x1b]21;nonsense=?\x07"),
        kitty_reply(&["unknown=bm9uc2Vuc2U"], "\x07")
    );
    let packet = b"\x1b]21;256=#fff;1=?;transparent_background_color9;transparent_background_color0=?;second_transparent_background=;2=?\x1b\\";
    assert_eq!(
        reply_to(&mut parser, packet),
        kitty_reply(
            &[
                "unknown=MjU2",
                "1=rgb:01/01/01",
                "unknown=dHJhbnNwYXJlbnRfYmFja2dyb3VuZF9jb2xvcjk",
                "unknown=dHJhbnNwYXJlbnRfYmFja2dyb3VuZF9jb2xvcjA",
                "unknown=c2Vjb25kX3RyYW5zcGFyZW50X2JhY2tncm91bmQ",
                "2=rgb:02/02/02",
            ],
            "\x1b\\"
        )
    );
    assert!(parser.color_overrides().is_none(), "no unknown key changed a color");
}

/// A set drops an `@opacity` suffix before reading the color, and bytes outside printable ASCII
/// are removed before a key is matched, so `fore` DEL `ground` reads as `foreground`.
#[test]
fn opacity_suffixes_and_unprintable_bytes_are_removed() {
    let mut parser = themed_parser();
    let _ = reply_to(&mut parser, b"\x1b]21;foreground=#ff0000@0.5\x07");
    assert_eq!(dynamic_override(&parser, DynamicColor::Foreground), Some((255, 0, 0)));
    assert_eq!(
        reply_to(&mut parser, b"\x1b]21;fore\x01ground=?;back\x7fground=?;cur\xc3\xa9sor=?\x07"),
        kitty_reply(
            &["foreground=rgb:ff/00/00", "background=rgb:0a/0a/0a", "cursor=rgb:fa/00/00"],
            "\x07"
        )
    );
}

/// An invalid spec, an empty field and an empty key are skipped: nothing changes, nothing is
/// answered, and the fields after them still apply.
#[test]
fn skipped_fields_change_nothing_and_the_rest_apply() {
    let mut parser = themed_parser();
    assert_eq!(
        reply_to(&mut parser, b"\x1b]21;;1=nope;;=?;=#fff;2=#020304;;\x07"),
        b"",
        "a set-only packet sends no reply"
    );
    assert_eq!(palette_override(&parser, 1), None);
    assert_eq!(palette_override(&parser, 2), Some((2, 3, 4)));
}

/// A bare `21`, an empty `21;`, and a packet cut by CAN, SUB or an ESC that does not start ST
/// apply nothing and answer nothing, and the parser returns to ground.
#[test]
fn bare_empty_and_aborted_packets_do_nothing() {
    for message in [
        &b"\x1b]21\x07"[..],
        b"\x1b]21\x1b\\",
        b"\x1b]21;\x07",
        b"\x1b]21;1=#fff;1=?\x18",
        b"\x1b]21;1=#fff;1=?\x1a",
        b"\x1b]21;1=#fff;1=?\x1b[m",
    ] {
        let mut parser = themed_parser();
        assert_eq!(reply_to(&mut parser, message), b"", "{message:?}");
        assert!(parser.color_overrides().is_none(), "{message:?}");
        parser.advance(b"Z");
        assert_eq!(parser.grid().row(0)[0].ch, 'Z', "{message:?} leaves the parser in ground");
        assert_eq!(
            reply_to(&mut parser, b"\x1b]21;1=?\x07"),
            kitty_reply(&["1=rgb:01/01/01"], "\x07"),
            "{message:?}: the next packet is read whole"
        );
    }
}

/// 526 counted fields are accepted; a 527th refuses the whole packet, with no state change and no
/// reply. Skipped fields (empty, invalid spec) never count, so 600 of them beside a query pass.
#[test]
fn the_item_budget_refuses_a_whole_packet_past_526_counted_fields() {
    let budget = "1=?;".repeat(525);
    let mut parser = themed_parser();
    let accepted = format!("\x1b]21;{budget}2=#020202\x07");
    let reply = reply_to(&mut parser, accepted.as_bytes());
    let answers = reply.windows(14).filter(|window| window == b"1=rgb:01/01/01").count();
    assert_eq!(answers, 525);
    assert_eq!(palette_override(&parser, 2), Some((2, 2, 2)), "the 526th field applies");
    for extra in ["3=#030303", "nonsense", "visual_bell=#fff", "4=?"] {
        let mut parser = themed_parser();
        let refused = format!("\x1b]21;2=#020202;{budget}{extra}\x07");
        assert_eq!(reply_to(&mut parser, refused.as_bytes()), b"", "{extra}");
        assert!(parser.color_overrides().is_none(), "{extra}: nothing before it applied");
    }
    let mut parser = themed_parser();
    let skipped = format!("\x1b]21;{}{}1=?\x07", ";".repeat(300), "1=nope;".repeat(300));
    assert_eq!(reply_to(&mut parser, skipped.as_bytes()), kitty_reply(&["1=rgb:01/01/01"], "\x07"));
}

/// The input cap is 16 KiB of payload: a packet at the cap applies, one byte more drops the whole
/// packet and leaves the parser in ground.
#[test]
fn a_packet_over_16_kib_is_dropped_whole() {
    let head = "1=#010101;1=?;";
    let fill = |len: usize| format!("{head}{}", ";".repeat(len - head.len()));
    let mut parser = themed_parser();
    let at_cap = format!("\x1b]21;{}\x07", fill(16 * 1024));
    assert_eq!(reply_to(&mut parser, at_cap.as_bytes()), kitty_reply(&["1=rgb:01/01/01"], "\x07"));
    let mut parser = themed_parser();
    let over_cap = format!("\x1b]21;{}\x07", fill(16 * 1024 + 1));
    assert_eq!(reply_to(&mut parser, over_cap.as_bytes()), b"");
    assert_eq!(palette_override(&parser, 1), None);
    parser.advance(b"Z");
    assert_eq!(parser.grid().row(0)[0].ch, 'Z');
}

/// The worst packets the caps admit (526 unknown keys filling 16 KiB, and 525 short queries beside
/// one long unknown key) each answer with one reply that fits one reply-spool submission.
#[test]
fn the_largest_reply_fits_one_spool_submission() {
    let payload_cap = 16 * 1024;
    let key_count = 526;
    // Keys of 30 or 31 bytes and their 525 separators fill the cap exactly.
    let key_bytes = payload_cap - (key_count - 1);
    let unknown_keys: Vec<String> = (0..key_count)
        .map(|index| "k".repeat(key_bytes / key_count + usize::from(index < key_bytes % key_count)))
        .collect();
    let all_unknown = unknown_keys.join(";");
    let long_key = "q".repeat(payload_cap - "0=?;".len() * 525);
    let queries_and_one_unknown = format!("{}{long_key}", "0=?;".repeat(525));
    for payload in [all_unknown, queries_and_one_unknown] {
        assert_eq!(payload.len(), payload_cap, "the packet fills the input cap");
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut parser = Parser::new_with_reply(Grid::new(8, 2), tx);
        parser.set_theme_palette_color(0, 1, 2, 3);
        parser.advance(format!("\x1b]21;{payload}\x1b\\").as_bytes());
        let reply = rx.try_recv().expect("one reply");
        assert!(rx.try_recv().is_err(), "the answer is a single reply");
        assert!(reply.starts_with(b"\x1b]21;") && reply.ends_with(b"\x1b\\"));
        assert!(reply.len() <= SPOOL_SUBMISSION_BYTES, "{} bytes", reply.len());
    }
}
