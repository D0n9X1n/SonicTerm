//! Pins OSC 133 framing and marker validation: the raw collector applies a marker only once BEL or
//! ST completes it, so a marker cut by CAN, SUB or an ESC that does not complete ST moves and
//! stores nothing; only the kinds A-D and L, N, P, I are read and L takes at most its token, both
//! before admission, so an unknown or malformed kind never latches; and a `\;` stays in its value.

use super::*;

const TOKEN: &str = "0123456789abcdef0123456789abcdef";

/// A parser for a pane whose shell was given `TOKEN`.
fn tokened() -> Parser {
    let mut parser = Parser::new(Grid::new(30, 6));
    parser.set_shell_token(Some(TOKEN.to_owned()));
    parser
}

/// The cursor as (row, column).
fn cursor(parser: &Parser) -> (u16, u16) {
    (parser.grid().cursor.row, parser.grid().cursor.col)
}

/// The command text the active screen's semantic state holds.
fn cmdline(parser: &Parser) -> Option<String> {
    parser.grid().semantic().cmdline.as_ref().map(|text| text.as_str().to_owned())
}

/// Unknown and malformed kinds are ignored before admission: carrying the pane's token, none
/// latches the pane, changes the observed shell state or moves the cursor; a tokened A still does.
#[test]
fn unknown_or_malformed_kinds_never_latch() {
    let mut parser = tokened();
    parser.advance(b"ab");
    for kind in ["X", "AB", "Lx", "Ninja", "a", ""] {
        parser.advance(format!("\x1b]133;{kind};sonicterm_token={TOKEN}\x07").as_bytes());
        assert!(!parser.shell_markers_authenticated(), "{kind:?} latched");
        assert_eq!(parser.shell_integration_state(), ShellIntegrationState::None, "{kind:?}");
        assert_eq!(cursor(&parser), (0, 2), "{kind:?} moved the cursor");
    }
    parser.advance(format!("\x1b]133;A;sonicterm_token={TOKEN}\x07").as_bytes());
    assert!(parser.shell_markers_authenticated());
}

/// A marker acts only once BEL or ST completes it: an L or N cut by CAN, SUB or an ESC that does
/// not start ST (here `ESC 7`, which saves the cursor) moves nothing, and an authenticated C cut the
/// same way stores no text and starts no command. Complete tokened and tokenless markers still act,
/// and an L with more than its one token is invalid.
#[test]
fn cut_markers_apply_nothing() {
    for kind in ["L", "N"] {
        for cut in ["\x18", "\x1a", "\x1b7"] {
            let mut parser = tokened();
            parser.advance(format!("ab\x1b]133;{kind}{cut}").as_bytes());
            assert_eq!(cursor(&parser), (0, 2), "{kind} cut by {cut:?} moved the cursor");
        }
    }
    let mut parser = tokened();
    parser.advance(format!("\x1b]133;A;sonicterm_token={TOKEN}\x07").as_bytes());
    for cut in ["\x18", "\x1a", "\x1b7"] {
        let marker = format!("\x1b]133;C;sonicterm_token={TOKEN};cmdline_url=ls{cut}");
        let events = parser.advance(marker.as_bytes());
        assert_eq!(cmdline(&parser), None, "a C cut by {cut:?} stored text");
        assert!(!events.iter().any(|event| matches!(event, VtEvent::Command(_))), "{cut:?}");
    }
    let marker = format!("\x1b]133;C;sonicterm_token={TOKEN};cmdline_url=ls\x1b\\");
    let events = parser.advance(marker.as_bytes());
    assert_eq!(cmdline(&parser), Some("ls".to_owned()));
    assert!(events.iter().any(|event| matches!(event, VtEvent::Command(CommandEvent::CmdStart))));
    // A latched pane refuses tokenless markers, so a pane with no token shows a tokenless L acts.
    let mut plain = Parser::new(Grid::new(30, 6));
    plain.advance(b"ab\x1b]133;L\x1b\\");
    assert_eq!(cursor(&plain), (1, 0));
    let mut parser = tokened();
    let doubled = format!("ab\x1b]133;L;sonicterm_token={TOKEN};sonicterm_token={TOKEN}\x07");
    parser.advance(doubled.as_bytes());
    assert_eq!(cursor(&parser), (0, 2), "an L with two parameters is invalid");
    parser.advance(format!("\x1b]133;L;sonicterm_token={TOKEN}\x07").as_bytes());
    assert_eq!(cursor(&parser), (1, 0));
}

/// bash's `%q` writes a semicolon as `\;`; the marker splits only at unescaped semicolons, so the
/// quoted `cmdline=` value reaches the decoder whole through `Parser::advance`.
#[test]
fn a_quoted_semicolon_stays_in_the_command_text() {
    let mut parser = tokened();
    parser.advance(format!("\x1b]133;A;sonicterm_token={TOKEN}\x07").as_bytes());
    let marker = format!("\x1b]133;C;sonicterm_token={TOKEN};cmdline=echo\\ a\\;b\x07");
    parser.advance(marker.as_bytes());
    assert_eq!(cmdline(&parser), Some("echo a;b".to_owned()));
}
