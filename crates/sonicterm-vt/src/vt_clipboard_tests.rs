use super::*;
use crate::vt::vt_test_support::*;

/// The payload-free stream events one `advance` raised, in order.
fn phases(events: Vec<VtEvent>) -> Vec<(u64, ClipboardPhase)> {
    events
        .into_iter()
        .filter_map(|event| match event {
            VtEvent::ClipboardStream { stream, phase } => Some((stream, phase)),
            _ => None,
        })
        .collect()
}

/// Every chunk queued for `stream`, concatenated.
fn drain(parser: &Parser, stream: u64) -> Vec<u8> {
    let inbox = parser.clipboard_inbox();
    let mut bytes = Vec::new();
    while let Some(chunk) = inbox.pop(stream) {
        bytes.extend_from_slice(&chunk);
    }
    bytes
}

/// A 4 MiB OSC 52 payload fed in small reads crosses the generic escape cap: it moves through the
/// inbox in chunks of at most 64 KiB, one `Data` event each, prints nothing, and ends with `End`.
#[test]
fn stream_4_mib_across_chunks() {
    let mut parser = Parser::new(Grid::new(80, 24));
    let payload = vec![b'A'; 4 * 1024 * 1024];
    let mut stream_bytes = b"\x1b]52;c;".to_vec();
    stream_bytes.extend_from_slice(&payload);
    stream_bytes.extend_from_slice(b"\x1b\\after");
    let mut received = 0usize;
    let mut ended = None;
    for read in stream_bytes.chunks(10_000) {
        for (stream, phase) in phases(parser.advance(read)) {
            match phase {
                ClipboardPhase::Data => {
                    let chunk = parser.clipboard_inbox().pop(stream).expect("one chunk per Data");
                    assert!(chunk.len() <= MAX_CLIPBOARD_CHUNK_BYTES);
                    received += chunk.len();
                }
                ClipboardPhase::End { kinds, query } => {
                    received += drain(&parser, stream).len();
                    ended = Some((kinds.as_bytes().to_vec(), query));
                }
                ClipboardPhase::Abort => panic!("a terminated stream never aborts"),
            }
        }
        // Between reads the parser holds at most one partial chunk.
        assert!(parser.retained_amount().bytes <= MAX_CLIPBOARD_CHUNK_BYTES + 64 * 1024);
    }
    assert_eq!(received, payload.len());
    assert_eq!(ended, Some((b"c".to_vec(), false)));
    assert_eq!(row_text(&parser, 0).trim_end(), "after");
}

/// A stream without BEL or ST never commits: it stays open, raises no `End`, and the host can
/// see it is open.
#[test]
fn no_commit_without_terminator() {
    let mut parser = Parser::new(Grid::new(80, 24));
    let events = phases(parser.advance(b"\x1b]52;c;aGVsbG8="));
    assert!(events.iter().all(|(_, phase)| !matches!(phase, ClipboardPhase::End { .. })));
    assert!(parser.open_clipboard_stream().is_some());
}

/// CAN, an unrelated ESC, and the host's expiry abort an open stream, release its queued chunks,
/// and swallow the rest of the OSC through its terminator so none of it prints.
#[test]
fn abort_can_esc_and_60s_expiry_release_and_swallow() {
    let full_chunk = vec![b'A'; MAX_CLIPBOARD_CHUNK_BYTES];
    // CAN: the queued chunk is released and the stream aborts.
    let mut parser = Parser::new(Grid::new(80, 24));
    let mut bytes = b"\x1b]52;c;".to_vec();
    bytes.extend_from_slice(&full_chunk);
    bytes.extend_from_slice(b"\x18ok");
    let events = phases(parser.advance(&bytes));
    let stream = events[0].0;
    assert_eq!(events.last().map(|(_, phase)| *phase), Some(ClipboardPhase::Abort));
    assert_eq!(parser.clipboard_inbox().pop(stream), None, "CAN released the chunk");
    assert_eq!(row_text(&parser, 0).trim_end(), "ok");
    // An unrelated ESC aborts and the rest is swallowed through BEL.
    let mut parser = Parser::new(Grid::new(80, 24));
    let events = phases(parser.advance(b"\x1b]52;c;QUJD\x1bxhidden\x07shown"));
    assert_eq!(events.last().map(|(_, phase)| *phase), Some(ClipboardPhase::Abort));
    assert_eq!(row_text(&parser, 0).trim_end(), "shown");
    // The host's expiry aborts the open stream and the rest is swallowed through ST.
    let mut parser = Parser::new(Grid::new(80, 24));
    let _ = parser.advance(&bytes[..bytes.len() - 3]);
    let open = parser.open_clipboard_stream().expect("open stream");
    assert_eq!(parser.abort_clipboard_stream(), Some(open));
    assert!(parser.clipboard_inbox().is_empty());
    assert_eq!(parser.open_clipboard_stream(), None);
    let _ = parser.advance(b"tail\x1b\\shown");
    assert_eq!(row_text(&parser, 0).trim_end(), "shown");
}

/// `52;c` with no data field, and `52;c;` with an empty one, both end with kind `c` and no data.
#[test]
fn bare_52_c_ends_without_data() {
    for bytes in [&b"\x1b]52;c\x07"[..], b"\x1b]52;c;\x1b\\"] {
        let mut parser = Parser::new(Grid::new(80, 24));
        let events = phases(parser.advance(bytes));
        assert_eq!(events.len(), 1, "{bytes:?}");
        let (stream, phase) = events[0];
        assert!(
            matches!(phase, ClipboardPhase::End { kinds, query: false } if kinds.as_bytes() == b"c")
        );
        assert!(drain(&parser, stream).is_empty());
    }
}

/// A `?` payload ends as a query and queues nothing, so the host can refuse the read.
#[test]
fn reads_end_as_query_without_data() {
    let mut parser = Parser::new(Grid::new(80, 24));
    let events = phases(parser.advance(b"\x1b]52;c;?\x07"));
    assert!(matches!(events[..], [(_, ClipboardPhase::End { query: true, .. })]));
    assert!(parser.clipboard_inbox().is_empty());
}

/// A kinds field longer than 16 bytes is swallowed to its terminator with no event.
#[test]
fn long_kinds_swallowed_to_terminator() {
    let mut parser = Parser::new(Grid::new(80, 24));
    let events = phases(parser.advance(b"\x1b]52;ccccccccccccccccc;QUJD\x07after"));
    assert!(events.is_empty());
    assert_eq!(row_text(&parser, 0).trim_end(), "after");
}

/// OSC 1337 `Copy=:` feeds the inbox as a `c` write with its key in any ASCII case; an empty or
/// `?` value and WezTerm's `Copy=;` spelling write nothing.
#[test]
fn iterm2_copy_case_and_rejections() {
    for bytes in [&b"\x1b]1337;Copy=:QUJD\x07"[..], b"\x1b]1337;cOpY=:QUJD\x1b\\"] {
        let mut parser = Parser::new(Grid::new(80, 24));
        let events = phases(parser.advance(bytes));
        let (stream, phase) = *events.last().expect("an end");
        assert!(
            matches!(phase, ClipboardPhase::End { kinds, query: false } if kinds.as_bytes() == b"c")
        );
        assert_eq!(drain(&parser, stream), b"QUJD");
    }
    for bytes in
        [&b"\x1b]1337;Copy=:\x07"[..], b"\x1b]1337;Copy=:?\x07", b"\x1b]1337;Copy=;QUJD\x07"]
    {
        let mut parser = Parser::new(Grid::new(80, 24));
        let ends = phases(parser.advance(bytes))
            .into_iter()
            .filter(|(_, phase)| matches!(phase, ClipboardPhase::End { query: false, .. }))
            .count();
        assert_eq!(ends, 0, "{bytes:?}");
        assert!(parser.clipboard_inbox().is_empty());
    }
}

/// Cloning a stream event copies no clipboard bytes: the event carries ids and kinds only.
#[test]
fn vt_event_clone_copies_no_payload() {
    let mut parser = Parser::new(Grid::new(80, 24));
    let events = parser.advance(b"\x1b]52;c;c2VjcmV0\x07");
    let printed = format!("{:?}", events.clone());
    assert!(!printed.contains("c2VjcmV0"), "{printed}");
    assert!(std::mem::size_of::<ClipboardPhase>() <= 24);
}

/// The inbox prints only lengths, never the queued bytes.
#[test]
fn inbox_debug_redacted() {
    let mut parser = Parser::new(Grid::new(80, 24));
    let _ = parser.advance(b"\x1b]52;c;c2VjcmV0");
    let _ = parser.advance(b"\x07");
    let printed = format!("{:?}", parser.clipboard_inbox());
    assert!(!printed.contains("c2VjcmV0"), "{printed}");
    assert!(printed.contains("queued_bytes"), "{printed}");
}

/// Contract: a pop for a stream the host abandoned takes nothing from a newer stream: the queue
/// is in stream order, so a stale request drops only its own older chunks and stops at a newer
/// one, which the next write still finds whole.
#[test]
fn stale_pop_leaves_a_newer_streams_chunks() {
    let inbox = ClipboardInbox::default();
    inbox.push(1, b"old".to_vec());
    inbox.push(2, b"new".to_vec());
    assert_eq!(inbox.pop(1), Some(b"old".to_vec()));
    assert_eq!(inbox.pop(1), None, "stream 1 has nothing more");
    assert_eq!(inbox.pop(2), Some(b"new".to_vec()), "the newer stream's chunk is still queued");
}
