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

/// A chunk the parser refuses (past the OSC cap) or a CAN/SUB cancels drops the pending
/// notification it continued, so a later chunk under the same id never completes it cut.
#[test]
fn a_refused_or_cancelled_chunk_drops_the_pending_notification() {
    let oversized = format!("\x1b]99;i=x:d=0:p=body;{}\x1b\\", "m".repeat(MAX_NOTIFY_BYTES + 1));
    for interruption in [oversized.into_bytes(), b"\x1b]99;i=x:d=0:p=body;mid\x18".to_vec()] {
        let mut parser = Parser::new(Grid::new(20, 4));
        assert!(notifications(&mut parser, b"\x1b]99;i=x:d=0:p=body;old\x1b\\").is_empty());
        assert!(notifications(&mut parser, &interruption).is_empty());
        assert_eq!(
            notifications(&mut parser, b"\x1b]99;i=x:p=body;new\x1b\\"),
            vec![(None, "new".into(), Some("x".into()), NotifyOccasion::Always)]
        );
    }
}

/// `cancel_capture` reports only what it frees: pending OSC 99 chunks stay when a title OSC in
/// flight is cancelled, so they are not counted as released; they are freed, and counted, when
/// the cancelled OSC is itself an OSC 99 chunk.
#[test]
fn cancel_counts_pending_chunks_only_when_it_frees_them() {
    let mut parser = Parser::new(Grid::new(20, 4));
    assert!(notifications(&mut parser, b"\x1b]99;i=r:d=0;Pending title\x1b\\").is_empty());
    let pending = parser.retained_amount().bytes;
    assert!(pending > 0, "the pending chunk is retained");
    parser.advance(b"\x1b]2;a title in flight");
    let in_flight = parser.retained_amount().bytes - pending;
    assert_eq!(parser.cancel_capture(), in_flight, "the pending chunk is not released");
    assert_eq!(parser.retained_amount().bytes, pending, "and it is still retained");
    parser.advance(b"\x07\x1b]99;i=r:p=body;more");
    let held = parser.retained_amount().bytes;
    assert_eq!(parser.cancel_capture(), held, "an OSC 99 in flight frees the pending chunks too");
    assert_eq!(parser.retained_amount().bytes, 0);
}
