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
    assert!(parser.performer.kitty_transfer.is_none(), "m=0 ends the aborted transfer");
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
    // q=2 needs no host reply, and a virtual (U=1) or C=1 transmit moves no cursor, so parsing
    // continues through later sequences in one prefix. A quiet direct a=T yields for its cursor move.
    for control in ["a=T,U=1", "a=T,C=1", "a=t"] {
        let (mut parser, _pool) = parser();
        let stream = format!("\x1b_G{control},f=100,i=7,q=2;AAAA\x1b\\next");
        let (consumed, events, _) = parser.advance_with_replies(stream.as_bytes());
        assert_eq!(consumed, stream.len(), "{control} does not yield");
        assert_eq!(kitty_events(events).len(), 1);
    }
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

/// A pool whose every floor reservation is already held by other parsers.
fn exhausted_pool() -> (Arc<CaptureStagingPool>, Vec<Parser>) {
    let pool = CaptureStagingPool::new();
    let mut holders = Vec::new();
    while pool.live_captures() < GUARANTEED_CONCURRENT_CAPTURES + 64 {
        let mut holder = Parser::new_with_staging_pool(Grid::new(4, 2), None, pool.clone());
        let before = pool.live_captures();
        holder.advance(b"\x1bPq#");
        if pool.live_captures() == before {
            // When: live_captures did not grow, admission is refused and the pool is exhausted.
            break;
        }
        holders.push(holder);
    }
    (pool, holders)
}

#[test]
fn a_delete_still_dispatches_when_staging_is_exhausted() {
    // A header-only command needs no staging, so a full pool must not swallow it.
    let (pool, _holders) = exhausted_pool();
    let mut parser = Parser::new_with_staging_pool(Grid::new(40, 10), None, pool);
    let media = kitty_events(parser.advance(b"\x1b_Ga=d,d=I,i=7,q=2\x1b\\"));
    assert_eq!(media.len(), 1);
    assert_eq!(
        KittyCommand::parse(media[0].metadata.as_bytes()).expect("parses").action,
        KittyAction::Delete
    );
}

#[test]
fn a_refused_first_chunk_swallows_its_tail_and_reports_once() {
    // With staging exhausted, a chunked transmit is refused once; its tail neither stages nor errors again.
    let (pool, _holders) = exhausted_pool();
    let mut parser = Parser::new_with_staging_pool(Grid::new(40, 10), None, pool);
    let (events, replies) = feed_with_replies(
        &mut parser,
        b"\x1b_Ga=T,f=100,i=7,m=1;AAAA\x1b\\\x1b_Gm=1;BBBB\x1b\\\x1b_Gm=0;CCCC\x1b\\",
    );
    assert!(kitty_events(events).is_empty());
    let text = String::from_utf8_lossy(&replies);
    assert_eq!(text.matches("i=7;").count(), 1, "one error for one transfer: {text:?}");
}

#[test]
fn a_refused_medium_with_more_chunks_reports_once() {
    // t=f with m=1 answers ENOTSUPPORTED once; the final chunk adds no second error.
    let (mut parser, _pool) = parser();
    let (_, replies) =
        feed_with_replies(&mut parser, b"\x1b_Ga=T,t=f,i=7,m=1;AAAA\x1b\\\x1b_Gm=0;BBBB\x1b\\");
    let text = String::from_utf8_lossy(&replies);
    assert_eq!(text.matches("i=7;").count(), 1, "{text:?}");
    assert!(text.contains("ENOTSUPPORTED"));
}

#[test]
fn can_in_the_first_chunk_aborts_the_transfer() {
    // CAN inside a chunked transfer's first chunk leaves its tail swallowed, not staged as a new image.
    let (mut parser, pool) = parser();
    let events =
        parser.advance(b"\x1b_Ga=T,f=100,i=7,q=2,m=1;AA\x18\x1b_Gm=1;BBBB\x1b\\\x1b_Gm=0;CC\x1b\\");
    assert!(kitty_events(events).is_empty());
    assert_eq!(pool.live_captures(), 0);
}

#[test]
fn cancelling_the_first_chunk_aborts_the_transfer() {
    // A host cancel during the first chunk swallows that chunk and every later one.
    let (mut parser, pool) = parser();
    parser.advance(b"\x1b_Ga=T,f=100,i=7,q=2,m=1;AAAA");
    assert!(parser.cancel_capture() > 0);
    let events = parser.advance(b"\x1b\\\x1b_Gm=1;BBBB\x1b\\\x1b_Gm=0;CC\x1b\\");
    assert!(kitty_events(events).is_empty());
    assert_eq!(pool.live_captures(), 0);
}

#[test]
fn an_over_cap_continuation_releases_staging_at_once() {
    // Once a continuation pushes the transfer past the cap, its staging is released and the rest swallowed.
    let (mut parser, pool) = parser();
    parser.advance(b"\x1b_Ga=T,f=100,i=7,m=1;AAAA\x1b\\");
    let mut chunk = b"\x1b_Gm=1;".to_vec();
    chunk.extend(std::iter::repeat_n(b'A', MAX_MEDIA_PAYLOAD_BYTES + 8));
    parser.advance(&chunk);
    assert_eq!(pool.live_captures(), 0, "nothing is staged while the chunk is still arriving");
    parser.advance(b"\x1b\\");
    assert_eq!(parser.retained_amount().bytes, 0);
}

#[test]
fn sixel_ending_an_advance_between_chunks_still_ends_the_upload() {
    // Even when a read ends exactly after ESC P q, the open upload yields before the next read.
    let (mut parser, _pool) = parser();
    parser.advance(b"\x1b_Ga=T,f=100,i=7,q=2,m=1;AAAA\x1b\\");
    parser.advance(b"\x1bPq");
    assert!(parser.live_capture_count() <= 1);
}

#[test]
fn chunk_quiet_settings_follow_the_latest_chunk() {
    // q on a middle chunk applies; q=0 on the final chunk restores replies.
    let (mut loud, _pool) = parser();
    let (events, _) = feed_with_replies(
        &mut loud,
        b"\x1b_Ga=T,f=100,i=7,m=1;AAAA\x1b\\\x1b_Gm=1,q=2;BBBB\x1b\\\x1b_Gm=0,q=0;\x1b\\",
    );
    let media = kitty_events(events);
    assert_eq!(KittyCommand::parse(media[0].metadata.as_bytes()).expect("parses").quiet, 0);
    let (mut quiet, _pool) = parser();
    let (events, _) = feed_with_replies(
        &mut quiet,
        b"\x1b_Ga=T,f=100,i=7,m=1;AAAA\x1b\\\x1b_Gm=1,q=2;BBBB\x1b\\\x1b_Gm=0;\x1b\\",
    );
    assert_eq!(
        KittyCommand::parse(kitty_events(events)[0].metadata.as_bytes()).expect("parses").quiet,
        2
    );
}

#[test]
fn an_invalid_command_naming_an_image_is_answered_and_not_dispatched() {
    // I= is refused: the client hears EINVAL for its image id, and nothing reaches the host to draw.
    let (mut parser, _pool) = parser();
    let (events, replies) = feed_with_replies(&mut parser, b"\x1b_Ga=T,I=7,i=3;AAAA\x1b\\");
    assert!(kitty_events(events).is_empty());
    assert!(String::from_utf8_lossy(&replies).contains("i=3;EINVAL"));
}

#[test]
fn an_orphan_continuation_is_swallowed() {
    // A continuation with no open transfer is not a new default transmit.
    let (mut parser, _pool) = parser();
    assert!(kitty_events(parser.advance(b"\x1b_Gm=0;AAAA\x1b\\")).is_empty());
}

#[test]
fn many_empty_chunks_keep_the_transfer_bounded() {
    // Thousands of payload-free `m=1,q=2;` chunks add nothing to what the transfer holds.
    let (mut parser, _pool) = parser();
    parser.advance(b"\x1b_Ga=T,f=100,i=7,m=1;AAAA\x1b\\");
    let before = parser.retained_amount().bytes;
    for _ in 0..5_000 {
        parser.advance(b"\x1b_Gm=1,q=2;\x1b\\");
    }
    assert_eq!(parser.retained_amount().bytes, before);
    let media = only_media(parser.advance(b"\x1b_Gm=0;BBBB\x1b\\"));
    assert!(media.metadata.len() < 128, "metadata stays one control section: {}", media.metadata);
}

#[test]
fn an_overlong_control_section_is_refused_not_cut_and_run() {
    // A valid delete padded past the bound, then an invalid key, must not delete anything.
    let (mut parser, _pool) = parser();
    let mut header = b"\x1b_Ga=d,d=I,i=7,".to_vec();
    while header.len() < MAX_KITTY_HEADER_BYTES + 3 {
        header.extend_from_slice(b"z=1,");
    }
    header.extend_from_slice(b"q=3\x1b\\");
    let (events, replies) = feed_with_replies(&mut parser, &header);
    assert!(kitty_events(events).is_empty(), "nothing is dispatched");
    assert!(String::from_utf8_lossy(&replies).contains("i=7;EINVAL"));
}

#[test]
fn unfinished_control_sections_reserve_no_staging() {
    // More open headers than the pool guarantees: none reserves, so a real image still stages.
    let pool = CaptureStagingPool::new();
    let mut headers: Vec<Parser> = (0..GUARANTEED_CONCURRENT_CAPTURES + 4)
        .map(|_| Parser::new_with_staging_pool(Grid::new(4, 2), None, pool.clone()))
        .collect();
    for parser in &mut headers {
        parser.advance(b"\x1b_Ga=T,f=100,i=7");
        assert_eq!(parser.live_capture_count(), 1, "the open APC is live for stall detection");
    }
    assert_eq!(pool.live_captures(), 0);
    let mut image = Parser::new_with_staging_pool(Grid::new(4, 2), None, pool.clone());
    assert_eq!(only_media(image.advance(b"\x1b_Ga=T,f=100,i=8,q=2;AAAA\x1b\\")).data, b"AAAA");
}

#[test]
fn an_over_cap_first_chunk_releases_staging_before_its_terminator() {
    // The first APC passing the payload cap is refused at once; nothing waits for ST.
    let (mut parser, pool) = parser();
    let mut chunk = b"\x1b_Ga=T,f=100,i=7;".to_vec();
    chunk.extend(std::iter::repeat_n(b'A', MAX_MEDIA_PAYLOAD_BYTES + 8));
    let (_, replies) = feed_with_replies(&mut parser, &chunk);
    assert_eq!(pool.live_captures(), 0);
    assert_eq!(parser.retained_amount().bytes, 0);
    assert!(String::from_utf8_lossy(&replies).contains("i=7;ENOMEM"));
}

#[test]
fn sixel_reserves_only_after_an_upload_released_its_own() {
    // With every other floor reservation held, a Sixel after an open upload still stages,
    // because the upload gives its reservation back first.
    let pool = CaptureStagingPool::new();
    let mut holders: Vec<Parser> = (0..GUARANTEED_CONCURRENT_CAPTURES - 1)
        .map(|_| Parser::new_with_staging_pool(Grid::new(4, 2), None, pool.clone()))
        .collect();
    for holder in &mut holders {
        holder.advance(b"\x1bPq#");
    }
    let mut parser = Parser::new_with_staging_pool(Grid::new(40, 10), None, pool.clone());
    parser.advance(b"\x1b_Ga=T,f=100,i=7,q=2,m=1;AAAA\x1b\\");
    let events = parser.advance(b"\x1bPq#0;2;100;0;0#0~\x1b\\");
    assert!(
        events.iter().any(
            |event| matches!(event, VtEvent::Media(media) if media.protocol == MediaProtocol::Sixel)
        ),
        "the Sixel is admitted, not refused"
    );
}

#[test]
fn cancelling_a_first_chunk_keeps_its_aborted_state() {
    // The cancel records the aborted transfer and does not erase it, so the tail reserves nothing.
    let (mut parser, pool) = parser();
    parser.advance(b"\x1b_Ga=T,f=100,i=7,m=1;AAAA");
    assert!(parser.cancel_capture() > 0);
    parser.advance(b"\x1b\\\x1b_Gm=1;BBBB");
    assert_eq!(pool.live_captures(), 0, "the tail is swallowed, not staged");
    let (_, replies) = feed_with_replies(&mut parser, b"\x1b\\\x1b_Gm=0;\x1b\\");
    assert!(
        String::from_utf8_lossy(&replies).contains("i=7;ECANCELED"),
        "the client hears it once"
    );
}

#[test]
fn a_header_only_middle_chunk_sets_quiet() {
    // `m=1,q=2` with no `;` still silences the rest of the transfer, including its cancellation.
    let (mut parser, _pool) = parser();
    parser.advance(b"\x1b_Ga=T,f=100,i=7,m=1;AAAA\x1b\\\x1b_Gm=1,q=2\x1b\\");
    parser.cancel_capture();
    let (_, replies) = feed_with_replies(&mut parser, b"\x1b_Gm=0;\x1b\\");
    assert!(replies.is_empty(), "q=2 silences the cancellation reply");
}

#[test]
fn can_in_a_continuation_header_aborts_the_upload() {
    // CAN while a continuation's control section is still arriving cancels the whole transfer:
    // the final chunk dispatches nothing and the upload's staging is released.
    let (mut parser, pool) = parser();
    parser.advance(b"\x1b_Ga=T,f=24,s=1,v=1,i=7,q=2,m=1;AAAA\x1b\\");
    parser.advance(b"\x1b_Gm=\x18");
    assert_eq!(pool.live_captures(), 0, "the cancelled upload's reservation is released");
    let events = parser.advance(b"\x1b_Gm=0;\x1b\\");
    assert!(kitty_events(events).is_empty(), "the original pixels never render");
}

#[test]
fn payload_free_chunks_still_advance_progress() {
    // A transfer receiving only empty `m=1` chunks is alive: its progress moves between samples.
    let (mut parser, _pool) = parser();
    parser.advance(b"\x1b_Ga=T,f=100,i=7,q=2,m=1;AAAA\x1b\\");
    let first = parser.capture_progress();
    parser.advance(b"\x1b_Gm=1,q=2;\x1b\\");
    let second = parser.capture_progress();
    assert!(second > first, "a completed empty chunk still counts as arriving bytes");
    parser.advance(b"\x1b_Gm=1,q=2;\x1b\\");
    assert!(parser.capture_progress() > second);
}
