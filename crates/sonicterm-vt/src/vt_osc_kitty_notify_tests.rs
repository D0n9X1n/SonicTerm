//! OSC 99 through the parser: herdr's notification forms reach the host as events, a query is
//! answered, and RIS drops pending chunks.

use super::*;

/// The notifications `bytes` produce, as (title, body, id, occasion).
fn notifications(
    parser: &mut Parser,
    bytes: &[u8],
) -> Vec<(Option<String>, String, Option<String>, NotifyOccasion)> {
    let mut found = Vec::new();
    let mut remaining = bytes;
    while !remaining.is_empty() {
        let (consumed, events, _) = parser.advance_with_replies(remaining);
        found.extend(events.into_iter().filter_map(|event| match event {
            VtEvent::Notify { title, body, id, occasion } => Some((title, body, id, occasion)),
            _ => None,
        }));
        remaining = &remaining[consumed..];
    }
    found
}

/// herdr's `OSC 9 ; text`, `OSC 99 ;; title` and two-chunk title + body each reach the host once.
#[test]
fn herdrs_notification_forms_reach_the_host() {
    let mut parser = Parser::new(Grid::new(20, 4));
    assert_eq!(
        notifications(&mut parser, b"\x1b]9;plain\x1b\\"),
        vec![(None, "plain".into(), None, NotifyOccasion::Always)]
    );
    assert_eq!(
        notifications(&mut parser, b"\x1b]99;;Title only\x1b\\"),
        vec![(Some("Title only".into()), String::new(), None, NotifyOccasion::Always)]
    );
    let chunked = b"\x1b]99;i=1:d=0;Done\x1b\\\x1b]99;i=1:p=body;3 tests\x1b\\";
    assert_eq!(
        notifications(&mut parser, chunked),
        vec![(Some("Done".into()), "3 tests".into(), Some("1".into()), NotifyOccasion::Always)]
    );
    assert_eq!(parser.grid().cursor.col, 0, "nothing printed");
}

/// A query is answered in kitty's form with the parts and occasions SonicTerm shows.
#[test]
fn a_query_is_answered() {
    let mut parser = Parser::new(Grid::new(20, 4));
    let (_, _, reply) = parser.advance_with_replies(b"\x1b]99;i=probe:p=?;\x1b\\");
    assert_eq!(reply, b"\x1b]99;i=probe:p=?;p=title,body:o=always,unfocused,invisible\x1b\\");
}

/// RIS drops a notification still receiving chunks, so its later body shows alone.
#[test]
fn reset_drops_pending_chunks() {
    let mut parser = Parser::new(Grid::new(20, 4));
    assert!(notifications(&mut parser, b"\x1b]99;i=r:d=0;Lost title\x1b\\\x1bc").is_empty());
    assert_eq!(
        notifications(&mut parser, b"\x1b]99;i=r:p=body;body\x1b\\"),
        vec![(None, "body".into(), Some("r".into()), NotifyOccasion::Always)]
    );
}
