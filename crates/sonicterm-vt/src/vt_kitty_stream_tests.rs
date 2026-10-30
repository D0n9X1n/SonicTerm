//! Pins the kitty APC stream: chunk reassembly into one reservation, refusal of non-direct media,
//! aborts by delete, other commands, CAN/SUB, cancellation and other media, and reply order.

use super::*;
use crate::vt::vt_test_support::*;

/// Feed `bytes` the way the VT worker does, collecting every reply in delivery order and the
/// events of each advance prefix.
fn feed_with_replies(parser: &mut Parser, bytes: &[u8]) -> (Vec<VtEvent>, Vec<u8>) {
    let mut events = Vec::new();
    let mut replies = Vec::new();
    let mut remaining = bytes;
    while !remaining.is_empty() {
        let (consumed, prefix_events, prefix_replies) = parser.advance_with_replies(remaining);
        events.extend(prefix_events);
        replies.extend(prefix_replies);
        remaining = &remaining[consumed..];
    }
    (events, replies)
}

/// A parser on a private pool, so admission and accounting are this test's alone.
fn parser() -> (Parser, Arc<CaptureStagingPool>) {
    let pool = CaptureStagingPool::new();
    (Parser::new_with_staging_pool(Grid::new(40, 10), None, pool.clone()), pool)
}

/// Kitty media events in `events`.
fn kitty_events(events: Vec<VtEvent>) -> Vec<MediaEvent> {
    events
        .into_iter()
        .filter_map(|event| match event {
            VtEvent::Media(media) if media.protocol == MediaProtocol::Kitty => Some(media),
            _ => None,
        })
        .collect()
}

#[test]
fn chunks_reassemble_into_one_event_with_the_first_header() {
    // Claude Code's shape: a=T,...,m=1 then m=1 chunks then m=0; the payloads concatenate in order.
    let (mut parser, _pool) = parser();
    let stream =
        b"\x1b_Ga=T,f=100,q=2,U=1,i=7,m=1;AAAA\x1b\\\x1b_Gm=1;BBBB\x1b\\\x1b_Gm=0;CCCC\x1b\\";
    let media = only_media(parser.advance(stream));
    assert_eq!(media.data, b"AAAABBBBCCCC");
    let command = KittyCommand::parse(media.metadata.as_bytes()).expect("merged header parses");
    assert_eq!(
        (command.action, command.image_id, command.quiet),
        (KittyAction::TransmitAndDisplay, 7, 2)
    );
    assert_eq!(parser.live_capture_count(), 0, "a finished transfer holds nothing");
}

#[test]
fn chunks_survive_every_buffer_split() {
    // A PTY read may cut anywhere, including inside a continuation header or its terminator.
    let stream = b"\x1b_Ga=T,f=100,i=7,q=2,m=1;AAAA\x1b\\\x1b_Gm=0;BBBB\x1b\\";
    for split in 0..=stream.len() {
        let (mut parser, _pool) = parser();
        let mut events = parser.advance(&stream[..split]);
        events.extend(parser.advance(&stream[split..]));
        assert_eq!(only_media(events).data, b"AAAABBBB", "split {split}");
    }
}

#[test]
fn text_between_chunks_is_printed() {
    // The protocol allows ordinary output between chunks; it reaches the grid as text.
    let (mut parser, _pool) = parser();
    let events = parser.advance(b"\x1b_Ga=T,f=100,i=7,q=2,m=1;AAAA\x1b\\hi\x1b_Gm=0;BBBB\x1b\\");
    assert_eq!(only_media(events).data, b"AAAABBBB");
    assert!(row_text(&parser, 0).starts_with("hi"));
}

#[test]
fn a_four_chunk_transfer_holds_one_reservation_throughout() {
    // Continuation chunks read their headers into scratch and stream into the transfer's capture,
    // so the pool never sees a second capture, including mid-header of a later chunk.
    let (mut parser, pool) = parser();
    parser.advance(b"\x1b_Ga=T,f=100,i=7,q=2,m=1;AAAA\x1b\\");
    for _ in 0..3 {
        parser.advance(b"\x1b_Gm=");
        assert_eq!(pool.live_captures(), 1, "mid-header of a continuation");
        assert_eq!(parser.live_capture_count(), 1);
        parser.advance(b"1;BBBB\x1b\\");
        assert_eq!(pool.live_captures(), 1, "between chunks");
    }
    parser.advance(b"\x1b_Gm=0;\x1b\\");
    assert_eq!(pool.live_captures(), 0);
}

#[test]
fn an_open_transfer_counts_as_retained_and_progressing() {
    // Between chunks the transfer is the parser's capture: its bytes are retained and its progress
    // advances with each chunk, so a healthy transfer never reads as stalled.
    let (mut parser, _pool) = parser();
    parser.advance(b"\x1b_Ga=T,f=100,i=7,q=2,m=1;AAAA\x1b\\");
    let first = parser.capture_progress();
    assert!(parser.retained_amount().bytes >= 4);
    parser.advance(b"\x1b_Gm=1;BBBBBBBB\x1b\\");
    assert!(parser.capture_progress() > first, "progress advances across chunks");
}

#[test]
fn file_temporary_and_shared_memory_transmissions_are_refused_unread() {
    // t=f/t/s reply ENOTSUPPORTED and dispatch nothing; the payload (a path) is never surfaced.
    for medium in ["f", "t", "s"] {
        let (mut parser, _pool) = parser();
        let stream = format!("\x1b_Ga=T,t={medium},i=9;L2V0Yy9wYXNzd2Q=\x1b\\");
        let (events, replies) = feed_with_replies(&mut parser, stream.as_bytes());
        assert!(kitty_events(events).is_empty(), "t={medium} dispatches nothing");
        assert!(String::from_utf8_lossy(&replies).contains("i=9;ENOTSUPPORTED"), "t={medium}");
    }
}

#[test]
fn a_refused_chunked_transfer_swallows_its_tail() {
    // A refused first chunk with m=1 leaves later chunks swallowed, never printed or dispatched.
    let (mut parser, _pool) = parser();
    let events = parser.advance(b"\x1b_Ga=T,t=f,i=9,q=2,m=1;AAAA\x1b\\\x1b_Gm=0;BBBB\x1b\\ok");
    assert!(kitty_events(events).is_empty());
    assert!(row_text(&parser, 0).starts_with("ok"));
    assert!(parser.kitty_transfer.is_none(), "m=0 ends the aborted transfer");
}

#[test]
fn a_delete_mid_upload_aborts_it_silently_and_is_processed() {
    // a=d ends an upload without an error reply, then dispatches itself.
    let (mut parser, pool) = parser();
    let (events, replies) = feed_with_replies(
        &mut parser,
        b"\x1b_Ga=T,f=100,i=7,m=1;AAAA\x1b\\\x1b_Ga=d,d=I,i=7\x1b\\",
    );
    let media = kitty_events(events);
    assert_eq!(media.len(), 1, "only the delete dispatches");
    assert_eq!(
        KittyCommand::parse(media[0].metadata.as_bytes()).expect("parses").action,
        KittyAction::Delete
    );
    assert!(replies.is_empty(), "abort by delete is silent");
    assert_eq!(pool.live_captures(), 0, "the upload's reservation is released");
}

#[test]
fn another_command_mid_upload_aborts_it_with_einval() {
    // A new transmit while an upload is open ends the upload with an error and then runs.
    let (mut parser, pool) = parser();
    let (events, replies) = feed_with_replies(
        &mut parser,
        b"\x1b_Ga=T,f=100,i=7,m=1;AAAA\x1b\\\x1b_Ga=T,f=100,i=8,q=2;BBBB\x1b\\",
    );
    assert_eq!(kitty_events(events)[0].data, b"BBBB");
    assert!(String::from_utf8_lossy(&replies).contains("i=7;EINVAL"));
    assert_eq!(pool.live_captures(), 0);
}

#[test]
fn can_inside_a_chunk_aborts_the_whole_transfer() {
    // CAN cancels the chunk and the upload: earlier bytes never complete into an image.
    let (mut parser, pool) = parser();
    let events = parser
        .advance(b"\x1b_Ga=T,f=100,i=7,q=2,m=1;AAAA\x1b\\\x1b_Gm=1;BB\x18\x1b_Gm=0;CCCC\x1b\\");
    assert!(kitty_events(events).is_empty());
    assert_eq!(pool.live_captures(), 0);
}

#[test]
fn cancelling_between_chunks_aborts_without_swallowing_text() {
    // The host cancels a stalled upload at ground: the next text prints, later chunks are swallowed.
    let (mut parser, pool) = parser();
    parser.advance(b"\x1b_Ga=T,f=100,i=7,q=2,m=1;AAAA\x1b\\");
    assert!(parser.cancel_capture() > 0);
    assert_eq!(pool.live_captures(), 0);
    let events = parser.advance(b"ok\x1b_Gm=0;BBBB\x1b\\");
    assert!(kitty_events(events).is_empty());
    assert!(row_text(&parser, 0).starts_with("ok"));
}

#[test]
fn sixel_between_chunks_ends_the_upload() {
    // Only one capture may be live: a Sixel starting between kitty chunks ends the kitty upload.
    let (mut parser, _pool) = parser();
    parser.advance(b"\x1b_Ga=T,f=100,i=7,q=2,m=1;AAAA\x1b\\\x1bPq#");
    assert!(parser.live_capture_count() <= 1);
    let events = parser.advance(b"\x1b\\\x1b_Gm=0;BBBB\x1b\\");
    assert!(kitty_events(events).is_empty(), "the aborted upload never completes");
}

#[test]
fn an_oversized_chunked_transfer_is_refused_whole() {
    // Past the payload cap the transfer is refused: no partial image, an error reply instead.
    let pool = CaptureStagingPool::new();
    let mut parser = Parser::new_with_staging_pool(Grid::new(40, 10), None, pool);
    let chunk = vec![b'A'; 4096];
    let mut stream = b"\x1b_Ga=T,f=100,i=7,m=1;".to_vec();
    stream.extend_from_slice(&chunk);
    stream.extend_from_slice(b"\x1b\\");
    let mut sent = chunk.len();
    while sent <= MAX_MEDIA_PAYLOAD_BYTES {
        stream.extend_from_slice(b"\x1b_Gm=1;");
        stream.extend_from_slice(&chunk);
        stream.extend_from_slice(b"\x1b\\");
        sent += chunk.len();
    }
    stream.extend_from_slice(b"\x1b_Gm=0;\x1b\\");
    let (events, replies) = feed_with_replies(&mut parser, &stream);
    assert!(kitty_events(events).is_empty());
    assert!(String::from_utf8_lossy(&replies).contains("i=7;ENOMEM"));
}

#[test]
fn a_query_ends_the_prefix_so_its_reply_precedes_later_replies() {
    // a=q followed by DA: the parser yields after the query so the host can answer it first.
    let (mut parser, _pool) = parser();
    let stream = b"\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\\x1b[c";
    let (consumed, events, replies) = parser.advance_with_replies(stream);
    assert_eq!(kitty_events(events).len(), 1, "the query is dispatched");
    assert!(replies.is_empty(), "no VT reply precedes the host's query reply");
    assert!(consumed < stream.len(), "the DA query is left for the next prefix");
    let (_, _, later) = parser.advance_with_replies(&stream[consumed..]);
    assert!(!later.is_empty(), "DA is answered after the yield");
}

#[test]
fn quiet_transmits_do_not_yield() {
    // q=2 needs no host reply, so parsing continues through later sequences in one prefix.
    let (mut parser, _pool) = parser();
    let stream = b"\x1b_Ga=T,f=100,i=7,q=2;AAAA\x1b\\next";
    let (consumed, events, _) = parser.advance_with_replies(stream);
    assert_eq!(consumed, stream.len());
    assert_eq!(kitty_events(events).len(), 1);
}

#[test]
fn a_header_only_place_or_delete_dispatches_without_staging_a_payload() {
    // a=p and a=d carry no data; they still reach the host as typed commands.
    let (mut parser, _pool) = parser();
    let media = only_media(parser.advance(b"\x1b_Ga=p,U=1,i=7,c=4,r=2\x1b\\"));
    assert!(media.data.is_empty());
    assert_eq!(
        KittyCommand::parse(media.metadata.as_bytes()).expect("parses").action,
        KittyAction::Place
    );
}
