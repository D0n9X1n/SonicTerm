//! Host reports a program waits for: DECRQM for every stored mode, the color-scheme query and its
//! mode 2031 updates, `OSC 4` for the whole 256-color palette, and the cell size in pixels.

use super::*;
use sonicterm_types::ColorScheme;

/// Feed `query` to `parser` until every byte is consumed and return the reply bytes. The parser
/// yields after a bounded batch of replies, and an OSC ended by ST yields before its final `\`,
/// so a query is fed as the VT worker feeds output.
fn reply_to(parser: &mut Parser, query: &[u8]) -> Vec<u8> {
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

/// DECRQM answers 1 while set and 2 while reset for each private mode the parser both honours and
/// stores, checked across a set then a reset of the mode itself.
#[test]
fn decrqm_reports_every_stored_private_mode() {
    for mode in [1u16, 7, 47, 67, 1000, 1002, 1003, 1004, 1006, 1047, 1049, 2004, 2026, 2031, 9001]
    {
        let mut parser = Parser::new(Grid::new(8, 2));
        parser.advance(format!("\x1b[?{mode}h").as_bytes());
        let query = format!("\x1b[?{mode}$p");
        assert_eq!(reply_to(&mut parser, query.as_bytes()), format!("\x1b[?{mode};1$y").as_bytes());
        parser.advance(format!("\x1b[?{mode}l").as_bytes());
        assert_eq!(reply_to(&mut parser, query.as_bytes()), format!("\x1b[?{mode};2$y").as_bytes());
    }
}

/// Autowrap starts set, so a fresh parser reports mode 7 as set before any program touches it.
#[test]
fn decrqm_reports_autowrap_set_by_default() {
    let mut parser = Parser::new(Grid::new(8, 2));
    assert_eq!(reply_to(&mut parser, b"\x1b[?7$p"), b"\x1b[?7;1$y");
}

/// The mouse modes share one tracking selection: only the selected mode reads as set.
#[test]
fn decrqm_reports_only_the_selected_mouse_mode() {
    let mut parser = Parser::new(Grid::new(8, 2));
    parser.advance(b"\x1b[?1000h\x1b[?1003h");
    assert_eq!(reply_to(&mut parser, b"\x1b[?1003$p"), b"\x1b[?1003;1$y");
    assert_eq!(reply_to(&mut parser, b"\x1b[?1000$p"), b"\x1b[?1000;2$y");
}

/// Each mouse encoding mode reports 1 while it is the active format and 2 otherwise, so a program
/// that probes 1005, 1006, 1015 or 1016 learns which one its reports use.
#[test]
fn decrqm_reports_the_active_mouse_encoding() {
    let mut parser = Parser::new(Grid::new(8, 2));
    let modes = [1005u16, 1006, 1015, 1016];
    for active in modes {
        parser.advance(format!("\x1b[?{active}h").as_bytes());
        for mode in modes {
            let state = if mode == active { 1 } else { 2 };
            let query = format!("\x1b[?{mode}$p");
            assert_eq!(
                reply_to(&mut parser, query.as_bytes()),
                format!("\x1b[?{mode};{state}$y").as_bytes(),
                "{active} active, {mode} asked"
            );
        }
    }
    parser.advance(b"\x1b[?1015l");
    for mode in modes {
        let query = format!("\x1b[?{mode}$p");
        assert_eq!(reply_to(&mut parser, query.as_bytes()), format!("\x1b[?{mode};2$y").as_bytes());
    }
}

/// A mode the parser does not store answers 0 (not recognized), so a program that probes it falls
/// back at once instead of waiting out a timeout; nothing changes.
#[test]
fn decrqm_answers_zero_for_modes_it_does_not_store() {
    let mut parser = Parser::new(Grid::new(8, 2));
    for mode in [6u16, 12, 25, 66, 1048, 2048, 4242] {
        let query = format!("\x1b[?{mode}$p");
        assert_eq!(reply_to(&mut parser, query.as_bytes()), format!("\x1b[?{mode};0$y").as_bytes());
    }
    assert_eq!(parser.grid().cursor.col, 0, "a query prints nothing");
    assert_eq!(
        reply_to(&mut parser, b"\x1b[?2026;1$p"),
        b"",
        "a request for two modes stays silent"
    );
}

/// DSR 996 answers the seeded theme's scheme and stays silent before any theme is seeded.
#[test]
fn color_scheme_query_answers_the_seeded_scheme() {
    let mut parser = Parser::new(Grid::new(8, 2));
    assert_eq!(reply_to(&mut parser, b"\x1b[?996n"), b"", "no theme, no claim");
    parser.set_theme_color_scheme(ColorScheme::Dark);
    assert_eq!(reply_to(&mut parser, b"\x1b[?996n"), b"\x1b[?997;1n");
    parser.set_theme_color_scheme(ColorScheme::Light);
    assert_eq!(reply_to(&mut parser, b"\x1b[?996n"), b"\x1b[?997;2n");
}

/// Seed `parser` with a dark theme whose background is `background`, then finish the update.
fn apply_theme(
    parser: &mut Parser,
    background: (u8, u8, u8),
    scheme: ColorScheme,
) -> Option<Vec<u8>> {
    parser.set_theme_bg(background.0, background.1, background.2);
    parser.set_theme_color_scheme(scheme);
    parser.finish_theme_update()
}

/// With mode 2031 set, a theme update that changes any reported color or the scheme yields one
/// unsolicited report for the host to send; the first update, at pane creation, yields none.
#[test]
fn mode_2031_reports_each_theme_change_but_not_the_first_seed() {
    let mut parser = Parser::new(Grid::new(8, 2));
    assert_eq!(apply_theme(&mut parser, (0, 0, 0), ColorScheme::Dark), None, "the creation seed");
    parser.advance(b"\x1b[?2031h");
    assert_eq!(
        apply_theme(&mut parser, (10, 10, 10), ColorScheme::Dark),
        Some(b"\x1b[?997;1n".to_vec()),
        "a dark theme replaced by another dark theme is still a palette change"
    );
    assert_eq!(
        apply_theme(&mut parser, (10, 10, 10), ColorScheme::Dark),
        None,
        "no change, no report"
    );
    assert_eq!(
        apply_theme(&mut parser, (250, 250, 250), ColorScheme::Light),
        Some(b"\x1b[?997;2n".to_vec())
    );
}

/// Without mode 2031 a theme change sends nothing, and a later `?2031h` does not replay it.
#[test]
fn theme_changes_stay_silent_without_mode_2031() {
    let mut parser = Parser::new(Grid::new(8, 2));
    assert_eq!(apply_theme(&mut parser, (0, 0, 0), ColorScheme::Dark), None);
    assert_eq!(apply_theme(&mut parser, (250, 250, 250), ColorScheme::Light), None);
    parser.advance(b"\x1b[?2031h");
    assert_eq!(apply_theme(&mut parser, (250, 250, 250), ColorScheme::Light), None);
}

/// `OSC 4` answers palette entries 16..=255 from the shared table the renderer draws with, and
/// a burst of all 256 queries, as herdr sends at startup, gets 256 replies in order.
#[test]
fn osc4_answers_the_whole_256_color_palette() {
    let mut parser = Parser::new(Grid::new(8, 2));
    for index in 0..16u8 {
        parser.set_theme_palette_color(index, index, index, index);
    }
    assert_eq!(
        reply_to(&mut parser, b"\x1b]4;196;?\x1b\\"),
        b"\x1b]4;196;rgb:ffff/0000/0000\x1b\\"
    );
    let burst: Vec<u8> =
        (0..=255u16).flat_map(|index| format!("\x1b]4;{index};?\x1b\\").into_bytes()).collect();
    let reply = reply_to(&mut parser, &burst);
    let expected: Vec<u8> = (0..=255u8)
        .flat_map(|index| {
            let (red, green, blue) =
                sonicterm_types::indexed_rgb(index).unwrap_or((index, index, index));
            format!(
                "\x1b]4;{index};rgb:{red:02x}{red:02x}/{green:02x}{green:02x}/{blue:02x}{blue:02x}\x1b\\"
            )
            .into_bytes()
        })
        .collect();
    assert_eq!(reply, expected);
}

/// `CSI 16 t` reports the cell size in whole physical pixels, height first; it stays silent until
/// the host reports a cell size, and other window-manipulation requests are never answered.
#[test]
fn cell_size_report_uses_the_host_cell_size() {
    let mut parser = Parser::new(Grid::new(8, 2));
    assert_eq!(reply_to(&mut parser, b"\x1b[16t"), b"", "no cell size yet");
    parser.set_cell_pixel_size(9.6, 20.4);
    assert_eq!(reply_to(&mut parser, b"\x1b[16t"), b"\x1b[6;20;10t");
    for ignored in [&b"\x1b[14t"[..], b"\x1b[18t", b"\x1b[14;2t", b"\x1b[16;1t"] {
        assert_eq!(reply_to(&mut parser, ignored), b"", "{ignored:?}");
    }
}

/// Contract: DECSET 5522 (paste events) is stored per parser and advertised: DECRQM answers 1
/// while set and 2 while reset; DECRST and RIS reset it.
#[test]
fn decrqm_reports_after_6c() {
    let mut parser = Parser::new(Grid::new(8, 2));
    assert!(!parser.paste_events_enabled(), "a fresh parser has the mode reset");
    assert_eq!(reply_to(&mut parser, b"\x1b[?5522$p"), b"\x1b[?5522;2$y");
    parser.advance(b"\x1b[?5522h");
    assert!(parser.paste_events_enabled());
    assert_eq!(reply_to(&mut parser, b"\x1b[?5522$p"), b"\x1b[?5522;1$y");
    parser.advance(b"\x1b[?5522l");
    assert!(!parser.paste_events_enabled(), "DECRST resets it");
    assert_eq!(reply_to(&mut parser, b"\x1b[?5522$p"), b"\x1b[?5522;2$y");
    parser.advance(b"\x1b[?5522h\x1bc");
    assert!(!parser.paste_events_enabled(), "RIS resets it");
}

/// Contract: the lock-free paste-events snapshot carries the set bit and a count of every set to
/// reset transition, so a host that sees one word after DECRST then DECSET in the same batch still
/// knows a reset happened; a DECRST while reset and a RIS while reset count nothing.
#[test]
fn paste_events_snapshot_counts_each_reset() {
    let mut parser = Parser::new(Grid::new(8, 2));
    let fresh = parser.paste_events_snapshot();
    assert_eq!(fresh & 1, 0, "reset at start");
    parser.advance(b"\x1b[?5522h");
    let set = parser.paste_events_snapshot();
    assert_eq!(set, fresh | 1, "DECSET sets only the set bit");
    parser.advance(b"\x1b[?5522l\x1b[?5522h");
    let again = parser.paste_events_snapshot();
    assert_eq!(again & 1, 1);
    assert_ne!(again, set, "a reset then set in one batch still moves the word");
    parser.advance(b"\x1bc");
    let after_ris = parser.paste_events_snapshot();
    assert_eq!(after_ris & 1, 0);
    assert_ne!(after_ris >> 1, again >> 1, "RIS of a set mode counts a reset");
    parser.advance(b"\x1b[?5522l\x1bc");
    assert_eq!(parser.paste_events_snapshot(), after_ris, "no transition while reset");
}
