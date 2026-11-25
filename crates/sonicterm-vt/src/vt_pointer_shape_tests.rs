//! OSC 22 pointer shapes: the name table, the per-screen stacks, the tracking fallback, queries
//! and the packet bounds.

use super::*;

/// Feed `bytes` until every byte is consumed and return the reply bytes, as the VT worker does.
fn reply_to(parser: &mut Parser, bytes: &[u8]) -> Vec<u8> {
    let mut reply = Vec::new();
    let mut remaining = bytes;
    while !remaining.is_empty() {
        let (consumed, _, replies) = parser.advance_with_replies(remaining);
        assert!(consumed > 0, "the parser makes progress on {bytes:?}");
        reply.extend(replies);
        remaining = &remaining[consumed..];
    }
    reply
}

/// A fresh parser on an 8x2 grid.
fn fresh_parser() -> Parser {
    Parser::new(Grid::new(8, 2))
}

/// The `__current__` answer, which names the active screen's top entry.
fn current(parser: &mut Parser) -> String {
    let reply = reply_to(parser, b"\x1b]22;?__current__\x07");
    let text = String::from_utf8(reply).expect("an ASCII reply");
    text.strip_prefix("\x1b]22;")
        .and_then(|rest| rest.strip_suffix('\x07'))
        .expect("an OSC 22 reply ended by BEL")
        .to_owned()
}

/// Each of the 34 canonical CSS names selects its own shape, and `__current__` answers it back.
#[test]
fn every_canonical_name_selects_its_shape() {
    assert_eq!(PointerShape::ALL.len(), 34);
    for shape in PointerShape::ALL {
        let mut parser = fresh_parser();
        parser.advance(format!("\x1b]22;{}\x07", shape.css_name()).as_bytes());
        assert_eq!(parser.pointer_shape(), shape, "{}", shape.css_name());
        assert_eq!(current(&mut parser), shape.css_name());
    }
}

/// Every accepted alias resolves to the shape the wiki table lists, including the two names kitty
/// and Ghostty disagree on: `cross` is a crosshair and `fleur` is the move arrow.
#[test]
fn every_alias_resolves_per_the_table() {
    use PointerShape as Shape;
    let table = [
        ("left_ptr", Shape::Default),
        ("arrow", Shape::Default),
        ("question_arrow", Shape::Help),
        ("whats_this", Shape::Help),
        ("hand", Shape::Pointer),
        ("hand2", Shape::Pointer),
        ("pointing_hand", Shape::Pointer),
        ("left_ptr_watch", Shape::Progress),
        ("half-busy", Shape::Progress),
        ("watch", Shape::Wait),
        ("clock", Shape::Wait),
        ("plus", Shape::Cell),
        ("cross", Shape::Crosshair),
        ("tcross", Shape::Crosshair),
        ("xterm", Shape::Text),
        ("ibeam", Shape::Text),
        ("beam", Shape::Text),
        ("dnd-link", Shape::Alias),
        ("dnd-copy", Shape::Copy),
        ("fleur", Shape::Move),
        ("dnd-move", Shape::Move),
        ("pointer-move", Shape::Move),
        ("dnd-no-drop", Shape::NoDrop),
        ("crossed_circle", Shape::NotAllowed),
        ("forbidden", Shape::NotAllowed),
        ("hand1", Shape::Grab),
        ("openhand", Shape::Grab),
        ("closedhand", Shape::Grabbing),
        ("dnd-none", Shape::Grabbing),
        ("top_side", Shape::NResize),
        ("right_side", Shape::EResize),
        ("bottom_side", Shape::SResize),
        ("left_side", Shape::WResize),
        ("top_right_corner", Shape::NeResize),
        ("top_left_corner", Shape::NwResize),
        ("bottom_right_corner", Shape::SeResize),
        ("bottom_left_corner", Shape::SwResize),
        ("sb_h_double_arrow", Shape::EwResize),
        ("split_h", Shape::EwResize),
        ("sb_v_double_arrow", Shape::NsResize),
        ("split_v", Shape::NsResize),
        ("size_bdiag", Shape::NeswResize),
        ("size-bdiag", Shape::NeswResize),
        ("size_fdiag", Shape::NwseResize),
        ("size-fdiag", Shape::NwseResize),
        ("zoom_in", Shape::ZoomIn),
        ("zoom_out", Shape::ZoomOut),
    ];
    for (alias, shape) in table {
        assert_eq!(PointerShape::from_name(alias.as_bytes()), Some(shape), "{alias}");
        let mut parser = fresh_parser();
        parser.advance(format!("\x1b]22;{alias}\x07").as_bytes());
        assert_eq!(parser.pointer_shape(), shape, "{alias}");
        // `__current__` always answers the canonical CSS name, never the alias.
        assert_eq!(current(&mut parser), shape.css_name(), "{alias}");
    }
}

/// An unknown name, including any name longer than 32 bytes, changes nothing.
#[test]
fn an_unknown_or_overlong_name_changes_nothing() {
    let mut parser = fresh_parser();
    parser.advance(b"\x1b]22;pointer\x07");
    let before = parser.pointer_shape_snapshot();
    parser.advance(b"\x1b]22;nonsense\x07");
    let overlong = "a".repeat(33);
    assert_eq!(PointerShape::from_name(overlong.as_bytes()), None);
    parser.advance(format!("\x1b]22;{overlong}\x07").as_bytes());
    parser.advance(format!("\x1b]22;>{overlong}\x07").as_bytes());
    assert_eq!(parser.pointer_shape(), PointerShape::Pointer);
    assert_eq!(parser.pointer_shape_snapshot(), before);
}

/// `=` on an empty stack pushes one entry, which `<` pops back to the fallback; `<` on an empty
/// stack is a no-op that moves no generation.
#[test]
fn set_pushes_on_an_empty_stack_and_pop_empties_it() {
    let mut parser = fresh_parser();
    parser.advance(b"\x1b]22;=wait\x07");
    assert_eq!(parser.pointer_shape(), PointerShape::Wait);
    parser.advance(b"\x1b]22;<\x07");
    assert_eq!(parser.pointer_shape(), PointerShape::Text);
    assert_eq!(current(&mut parser), "0");
    let empty = parser.pointer_shape_snapshot();
    parser.advance(b"\x1b]22;<\x07");
    assert_eq!(parser.pointer_shape_snapshot(), empty);
}

/// The stack holds 16 entries: a 17th push evicts the bottom one, so after 15 pops the second
/// shape pushed is on top and one more pop empties the stack.
#[test]
fn a_seventeenth_push_evicts_the_bottom_entry() {
    let mut parser = fresh_parser();
    let pushed: Vec<PointerShape> = PointerShape::ALL[..17].to_vec();
    for shape in &pushed {
        parser.advance(format!("\x1b]22;>{}\x07", shape.css_name()).as_bytes());
    }
    assert_eq!(parser.pointer_shape(), pushed[16]);
    for _ in 0..15 {
        parser.advance(b"\x1b]22;<\x07");
    }
    assert_eq!(current(&mut parser), pushed[1].css_name());
    parser.advance(b"\x1b]22;<\x07");
    assert_eq!(current(&mut parser), "0");
}

/// `>` pushes every known name in order and skips empty and unknown ones; `<` ignores its names.
#[test]
fn push_takes_known_names_and_pop_ignores_names() {
    let mut parser = fresh_parser();
    parser.advance(b"\x1b]22;>wait,,nonsense,help\x07");
    assert_eq!(parser.pointer_shape(), PointerShape::Help);
    parser.advance(b"\x1b]22;<help,wait\x07");
    assert_eq!(parser.pointer_shape(), PointerShape::Wait);
    parser.advance(b"\x1b]22;<\x07");
    assert_eq!(current(&mut parser), "0");
}

/// An empty `22;` stores an unset top over an explicit shape, and popping that entry restores
/// the shape beneath it.
#[test]
fn an_empty_name_unsets_the_top_and_pop_restores_the_shape_beneath() {
    let mut parser = fresh_parser();
    parser.advance(b"\x1b]22;pointer\x07\x1b]22;>wait\x07");
    parser.advance(b"\x1b]22;\x07");
    assert_eq!(parser.pointer_shape(), PointerShape::Text);
    assert_eq!(current(&mut parser), "0");
    parser.advance(b"\x1b]22;<\x07");
    assert_eq!(parser.pointer_shape(), PointerShape::Pointer);
}

/// A bare `OSC 22`, ended by BEL or ST, is an empty name and unsets the top entry.
#[test]
fn a_bare_osc_22_unsets_the_top() {
    for bare in [b"\x1b]22\x07".as_slice(), b"\x1b]22\x1b\\"] {
        let mut parser = fresh_parser();
        parser.advance(b"\x1b]22;pointer\x07");
        reply_to(&mut parser, bare);
        assert_eq!(parser.pointer_shape(), PointerShape::Text, "{bare:?}");
        assert_eq!(current(&mut parser), "0");
    }
}

/// With an empty stack or an unset top, the fallback is `default` while 1000, 1002 or 1003 is on
/// and `text` otherwise; tracking never writes the stack, so an explicit shape survives it.
#[test]
fn tracking_modes_decide_only_the_fallback() {
    for mode in [1000u16, 1002, 1003] {
        let mut parser = fresh_parser();
        assert_eq!(parser.pointer_shape(), PointerShape::Text);
        parser.advance(format!("\x1b[?{mode}h").as_bytes());
        assert_eq!(parser.pointer_shape(), PointerShape::Default, "{mode}");
        parser.advance(b"\x1b]22;crosshair\x07");
        parser.advance(format!("\x1b[?{mode}l\x1b[?{mode}h").as_bytes());
        assert_eq!(parser.pointer_shape(), PointerShape::Crosshair, "{mode}");
        parser.advance(b"\x1b]22;\x07");
        assert_eq!(parser.pointer_shape(), PointerShape::Default, "{mode}");
        parser.advance(format!("\x1b[?{mode}l").as_bytes());
        assert_eq!(parser.pointer_shape(), PointerShape::Text, "{mode}");
    }
}

/// A query answers each name in order (`1` for a known name, the CSS name or `0` for
/// `__current__`, `text` and `default` for the fixed names, `0` otherwise) with the request's own
/// terminator, and changes no state or generation.
#[test]
fn a_query_answers_in_order_with_the_request_terminator_and_changes_nothing() {
    let mut parser = fresh_parser();
    parser.advance(b"\x1b]22;hand\x07");
    let before = parser.pointer_shape_snapshot();
    let query = "?pointer,nonsense,__current__,__default__,__grabbed__";
    assert_eq!(
        reply_to(&mut parser, format!("\x1b]22;{query}\x07").as_bytes()),
        b"\x1b]22;1,0,pointer,text,default\x07"
    );
    assert_eq!(
        reply_to(&mut parser, format!("\x1b]22;{query}\x1b\\").as_bytes()),
        b"\x1b]22;1,0,pointer,text,default\x1b\\"
    );
    assert_eq!(reply_to(&mut parser, b"\x1b]22;?\x07"), b"\x1b]22;0\x07");
    assert_eq!(parser.pointer_shape(), PointerShape::Pointer);
    assert_eq!(parser.pointer_shape_snapshot(), before);
}

/// `__current__` answers `0` for an empty stack and for an unset top, whatever the fallback.
#[test]
fn current_answers_zero_for_an_empty_stack_or_an_unset_top() {
    let mut parser = fresh_parser();
    parser.advance(b"\x1b[?1000h");
    assert_eq!(current(&mut parser), "0");
    parser.advance(b"\x1b]22;wait\x07\x1b]22;\x07");
    assert_eq!(current(&mut parser), "0");
}

/// Main and alternate screens keep separate stacks, a screen switch changes the effective shape,
/// and RIS empties both.
#[test]
fn each_screen_has_its_own_stack_and_ris_empties_both() {
    let mut parser = fresh_parser();
    parser.advance(b"\x1b]22;pointer\x07");
    let main = parser.pointer_shape_snapshot();
    parser.advance(b"\x1b[?1049h");
    assert_eq!(parser.pointer_shape(), PointerShape::Text);
    assert_ne!(parser.pointer_shape_snapshot(), main);
    parser.advance(b"\x1b]22;wait\x07");
    assert_eq!(parser.pointer_shape(), PointerShape::Wait);
    parser.advance(b"\x1b[?1049l");
    assert_eq!(parser.pointer_shape(), PointerShape::Pointer);
    parser.advance(b"\x1b[?1049h");
    assert_eq!(parser.pointer_shape(), PointerShape::Wait);
    parser.advance(b"\x1bc");
    assert_eq!(parser.pointer_shape(), PointerShape::Text);
    assert_eq!(current(&mut parser), "0");
    parser.advance(b"\x1b[?1049h");
    assert_eq!(current(&mut parser), "0");
}

/// Only the first 64 names of a packet are read: a 65th set is ignored and a 65-name query
/// answers 64 times.
#[test]
fn names_past_the_sixty_fourth_are_ignored() {
    let mut parser = fresh_parser();
    let names = vec!["wait"; 64].join(",");
    parser.advance(format!("\x1b]22;{names},pointer\x07").as_bytes());
    assert_eq!(parser.pointer_shape(), PointerShape::Wait);
    let queries = vec!["pointer"; 65].join(",");
    let reply = reply_to(&mut parser, format!("\x1b]22;?{queries}\x07").as_bytes());
    let answers = vec!["1"; 64].join(",");
    assert_eq!(reply, format!("\x1b]22;{answers}\x07").into_bytes());
}

/// The longest possible reply, 64 `__current__` answers of `vertical-text` ended by ST, stays at
/// most 903 bytes, far below the 32 KiB reply spool limit.
#[test]
fn the_longest_query_reply_stays_within_its_bound() {
    let mut parser = fresh_parser();
    parser.advance(b"\x1b]22;vertical-text\x07");
    let queries = vec!["__current__"; 64].join(",");
    let reply = reply_to(&mut parser, format!("\x1b]22;?{queries}\x1b\\").as_bytes());
    assert_eq!(reply.len(), 5 + 64 * 13 + 63 + 2);
    assert!(reply.len() <= 5 + 64 * 14 + 2);
    assert!(reply.len() < 32 * 1024);
}

/// A payload of 4096 bytes applies; one byte more is dropped whole and the text after it prints.
#[test]
fn a_packet_over_four_kib_is_dropped_whole() {
    let fill = |len: usize| format!("pointer,{}", "x".repeat(len - "pointer,".len()));
    let mut parser = fresh_parser();
    parser.advance(format!("\x1b]22;{}\x07", fill(4096)).as_bytes());
    assert_eq!(parser.pointer_shape(), PointerShape::Pointer);

    let mut parser = fresh_parser();
    parser.advance(format!("\x1b]22;{}\x07ok", fill(4097)).as_bytes());
    assert_eq!(parser.pointer_shape(), PointerShape::Text);
    assert!(vt_test_support::row_text(&parser, 0).starts_with("ok"));
}

/// A packet cut by CAN applies nothing.
#[test]
fn an_aborted_packet_applies_nothing() {
    let mut parser = fresh_parser();
    parser.advance(b"\x1b]22;pointer\x18");
    assert_eq!(parser.pointer_shape(), PointerShape::Text);
}

/// The published generation moves only when the effective shape really changes: a set to the
/// current shape, a repeated set, a pop on an empty stack and a query all leave it alone.
#[test]
fn the_generation_moves_only_on_a_real_change() {
    let mut parser = fresh_parser();
    let start = parser.pointer_shape_snapshot();
    parser.advance(b"\x1b]22;text\x07");
    assert_eq!(parser.pointer_shape_snapshot(), start);
    parser.advance(b"\x1b]22;pointer\x07");
    let pointer = parser.pointer_shape_snapshot();
    assert_ne!(pointer, start);
    parser.advance(b"\x1b]22;pointer\x07\x1b]22;=hand\x07");
    reply_to(&mut parser, b"\x1b]22;?__current__\x07");
    assert_eq!(parser.pointer_shape_snapshot(), pointer);
    parser.advance(b"\x1b[?1000h");
    assert_eq!(parser.pointer_shape_snapshot(), pointer);
}
