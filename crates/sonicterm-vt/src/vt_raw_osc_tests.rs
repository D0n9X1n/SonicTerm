use super::*;
use crate::vt::vt_test_support::*;

fn iterm2_sequence(payload_bytes: usize) -> Vec<u8> {
    let mut sequence = Vec::with_capacity(payload_bytes + 32);
    sequence.extend_from_slice(b"\x1b]1337;File=inline=1:");
    sequence.resize(sequence.len() + payload_bytes, b'A');
    sequence.extend_from_slice(b"\x1b\\");
    sequence
}

/// An iTerm2 payload larger than the generic OSC ceiling but below the media
/// ceiling must be staged whole and dispatched through the media contract.
#[test]
fn iterm2_payload_above_generic_escape_cap_dispatches() {
    let pool = CaptureStagingPool::new();
    let payload_bytes = 2 * MAX_ESCAPE_SEQUENCE_BYTES;
    let mut parser = Parser::new_with_staging_pool(Grid::new(80, 24), None, pool.clone());

    let media = only_media(parser.advance(&iterm2_sequence(payload_bytes)));

    assert_eq!(media.protocol, MediaProtocol::Iterm2File);
    assert_eq!(media.metadata, "File=inline=1");
    assert_eq!(media.data.len(), payload_bytes);
}

/// iTerm2 obeys the advertised payload boundary exactly: one lone capture can
/// dispatch 16 MiB, while the next byte rejects the whole image.
#[test]
fn iterm2_payload_uses_exact_media_boundary() {
    let pool = CaptureStagingPool::new();

    for payload_bytes in
        [MAX_ESCAPE_SEQUENCE_BYTES, 2 * MAX_ESCAPE_SEQUENCE_BYTES, MAX_MEDIA_PAYLOAD_BYTES]
    {
        let mut parser = Parser::new_with_staging_pool(Grid::new(80, 24), None, pool.clone());
        let media = only_media(parser.advance(&iterm2_sequence(payload_bytes)));
        assert_eq!(media.protocol, MediaProtocol::Iterm2File);
        assert_eq!(media.data.len(), payload_bytes);
    }

    let mut parser = Parser::new_with_staging_pool(Grid::new(80, 24), None, pool.clone());
    let events = parser.advance(&iterm2_sequence(MAX_MEDIA_PAYLOAD_BYTES + 1));
    assert!(events.iter().all(|event| !matches!(event, VtEvent::Media(_))));
}

/// The streamed OSC path must retain its terminator state across every input
/// split instead of requiring the whole introducer or ST in one PTY chunk.
#[test]
fn iterm2_media_supports_every_buffer_split() {
    let pool = CaptureStagingPool::new();
    let sequence = iterm2_sequence(8);

    for split in 0..=sequence.len() {
        let mut parser = Parser::new_with_staging_pool(Grid::new(80, 24), None, pool.clone());
        let mut events = parser.advance(&sequence[..split]);
        events.extend(parser.advance(&sequence[split..]));

        let media = only_media(events);
        assert_eq!(media.protocol, MediaProtocol::Iterm2File, "split {split}");
        assert_eq!(media.data, b"AAAAAAAA", "split {split}");
    }
}

/// iTerm2 metadata may fill its explicit cap, while one byte beyond rejects the
/// whole transfer rather than retaining unbounded attributes.
#[test]
fn iterm2_metadata_uses_exact_bounded_capacity() {
    let pool = CaptureStagingPool::new();
    for (metadata_bytes, should_dispatch) in [
        (MAX_ITERM2_METADATA_BYTES - b"File=".len(), true),
        (MAX_ITERM2_METADATA_BYTES - b"File=".len() + 1, false),
    ] {
        let mut sequence = b"\x1b]1337;File=".to_vec();
        sequence.extend(std::iter::repeat_n(b'a', metadata_bytes));
        sequence.extend_from_slice(b":AAAA\x1b\\");
        let mut parser = Parser::new_with_staging_pool(Grid::new(16, 2), None, pool.clone());

        let events = parser.advance(&sequence);
        let dispatched = events.iter().any(|event| matches!(event, VtEvent::Media(_)));

        assert_eq!(dispatched, should_dispatch, "metadata bytes {metadata_bytes}");
        assert_eq!(parser.live_capture_count(), 0);
    }
}

/// A recognized iTerm2 prefix without the metadata/data separator is malformed;
/// termination releases staging without surfacing a partial image.
#[test]
fn malformed_iterm2_media_is_rejected_and_resynchronizes() {
    let pool = CaptureStagingPool::new();
    let mut parser = Parser::new_with_staging_pool(Grid::new(16, 2), None, pool.clone());

    let events = parser.advance(b"\x1b]1337;File=inline=1\x1b\\Z");

    assert!(events.iter().all(|event| !matches!(event, VtEvent::Media(_))));
    assert_eq!(parser.grid().row(0)[0].ch, 'Z');
    assert_eq!(parser.live_capture_count(), 0);
}

/// CAN and SUB abort iTerm2 staging without dispatching the bytes accumulated
/// before cancellation as an image.
#[test]
fn can_and_sub_cancel_iterm2_without_emitting_media() {
    let pool = CaptureStagingPool::new();
    for cancel in [0x18, 0x1a] {
        let mut parser = Parser::new_with_staging_pool(Grid::new(16, 2), None, pool.clone());
        let mut sequence = b"\x1b]1337;File=inline=1:AAAA".to_vec();
        sequence.push(cancel);

        let events = parser.advance(&sequence);

        assert!(events.iter().all(|event| !matches!(event, VtEvent::Media(_))));
        assert_eq!(parser.live_capture_count(), 0);
    }
}

/// iTerm2 staging consumes the same admission slots and reports the same
/// progress/retention metrics as APC and DCS captures.
#[test]
fn iterm2_capture_participates_in_process_staging_accounting() {
    let pool = CaptureStagingPool::new();
    let mut parser = Parser::new_with_staging_pool(Grid::new(80, 24), None, pool.clone());

    parser.advance(b"\x1b]1337;File=inline=1:AAAA");

    assert_eq!(parser.live_capture_count(), 1);
    assert_eq!(parser.capture_progress(), b"File=inline=1".len() + 4);
    assert_eq!(pool.floor_reserved(), MIN_CAPTURE_STAGING_BYTES);
    assert_eq!(parser.retained_amount().items, 1);

    parser.advance(b"\x1b\\");
    assert_eq!(parser.live_capture_count(), 0);
    assert_eq!(parser.retained_amount().items, 0);
}

/// Host cancellation releases iTerm2 staging, swallows the sender's remaining
/// base64 tail, and resumes at ST without printing the abandoned transfer.
#[test]
fn host_cancelled_iterm2_capture_discards_tail_through_terminator() {
    let pool = CaptureStagingPool::new();
    let mut parser = Parser::new_with_staging_pool(Grid::new(16, 2), None, pool.clone());
    parser.advance(b"\x1b]1337;File=inline=1:AAAA");
    assert_eq!(parser.live_capture_count(), 1);

    assert!(parser.cancel_capture() > 0);
    parser.advance(b"BBBBCCCC\x1b\\Z");

    assert_eq!(parser.grid().row(0)[0].ch, 'Z');
    assert_eq!(parser.live_capture_count(), 0);
}

/// Kitty, Sixel, and iTerm2 all reserve from the same fixed floor pool, so a
/// mixed workload cannot admit more captures than the documented guarantee.
#[test]
fn mixed_media_protocols_share_the_process_staging_pool() {
    let pool = CaptureStagingPool::new();
    let mut parsers: Vec<Parser> = (0..GUARANTEED_CONCURRENT_CAPTURES)
        .map(|_| Parser::new_with_staging_pool(Grid::new(8, 1), None, pool.clone()))
        .collect();

    for (index, parser) in parsers.iter_mut().enumerate() {
        match index % 3 {
            0 => {
                parser.advance(b"\x1b_GA");
            }
            1 => {
                parser.advance(b"\x1bPq?");
            }
            _ => {
                parser.advance(b"\x1b]1337;File=inline=1:A");
            }
        }
        assert_eq!(parser.live_capture_count(), 1);
    }

    assert_eq!(pool.floor_reserved(), GUARANTEED_CONCURRENT_CAPTURES * MIN_CAPTURE_STAGING_BYTES);
    let mut refused = Parser::new_with_staging_pool(Grid::new(8, 1), None, pool.clone());
    refused.advance(b"\x1b]1337;File=inline=1:A");
    assert_eq!(refused.live_capture_count(), 1);
    assert_eq!(refused.retained_amount().bytes, 0);
}
