//! DECSCUSR: a program's cursor shape and steadiness request, kept per parser and reported to the
//! host as an event.

use super::*;
use sonicterm_types::{CursorRequest, CursorRequestShape};

/// The cursor-style events `bytes` produce.
fn style_events(parser: &mut Parser, bytes: &[u8]) -> Vec<Option<CursorRequest>> {
    let (_, events, _) = parser.advance_with_replies(bytes);
    events
        .into_iter()
        .filter_map(|event| match event {
            VtEvent::CursorStyle(request) => Some(request),
            _ => None,
        })
        .collect()
}

/// `CSI Ps SP q` stores the request and reports it; `CSI 0 SP q` and an omitted value restore the
/// configured cursor.
#[test]
fn decscusr_sets_and_restores_the_request() {
    let mut parser = Parser::new(Grid::new(8, 2));
    assert_eq!(parser.cursor_request(), None);
    let steady_bar = CursorRequest { shape: CursorRequestShape::Bar, steady: true };
    assert_eq!(style_events(&mut parser, b"\x1b[6 q"), vec![Some(steady_bar)]);
    assert_eq!(parser.cursor_request(), Some(steady_bar));
    assert_eq!(style_events(&mut parser, b"\x1b[0 q"), vec![None]);
    assert_eq!(parser.cursor_request(), None);
    parser.advance(b"\x1b[3 q");
    assert_eq!(style_events(&mut parser, b"\x1b[ q"), vec![None], "an omitted value is 0");
}

/// A value past 6 changes nothing and reports nothing; DECSCA (`CSI 1 " q`) is not a cursor style.
#[test]
fn decscusr_ignores_unknown_values_and_other_intermediates() {
    let mut parser = Parser::new(Grid::new(8, 2));
    parser.advance(b"\x1b[2 q");
    assert_eq!(style_events(&mut parser, b"\x1b[7 q"), vec![]);
    assert_eq!(style_events(&mut parser, b"\x1b[1\"q"), vec![]);
    let steady_block = CursorRequest { shape: CursorRequestShape::Block, steady: true };
    assert_eq!(parser.cursor_request(), Some(steady_block));
    assert_eq!(parser.grid().cursor.col, 0, "nothing printed");
}

/// RIS clears a request and reports the configured cursor again.
#[test]
fn reset_restores_the_configured_cursor() {
    let mut parser = Parser::new(Grid::new(8, 2));
    parser.advance(b"\x1b[5 q");
    assert_eq!(style_events(&mut parser, b"\x1bc"), vec![None]);
    assert_eq!(parser.cursor_request(), None);
}
