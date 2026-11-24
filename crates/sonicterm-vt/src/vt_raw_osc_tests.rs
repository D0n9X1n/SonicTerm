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
                // A kitty image payload; a bare control section would reserve nothing.
                parser.advance(b"\x1b_Gf=100;A");
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

/// A host cancel that reclaims an open kitty upload while an over-cap title is being refused
/// keeps the refusal: the rest of the title, its newline and a UTF-8 `“` (which holds 0x9c) are
/// swallowed up to its BEL, and the upload's staging is released.
#[test]
fn reclaiming_an_upload_keeps_a_refused_title_swallowing() {
    let pool = CaptureStagingPool::new();
    let mut parser = Parser::new_with_staging_pool(Grid::new(16, 2), None, pool.clone());
    parser.advance(b"\x1b_Ga=T,f=100,i=7,q=2,m=1;AAAA\x1b\\");
    let mut title = b"\x1b]2;".to_vec();
    title.resize(title.len() + MAX_SEMANTIC_OSC_BYTES + 1, b't');
    parser.advance(&title);
    assert!(matches!(parser.raw_osc, Some(RawOsc::Refused { .. })), "precondition: refused");
    assert!(parser.cancel_capture() > 0, "the upload's bytes are released");
    assert_eq!(pool.live_captures(), 0);
    parser.advance("tail\n\u{201c}more\x07Z".as_bytes());
    assert_eq!(parser.grid().row(0)[0].ch, 'Z', "no byte of the refused title printed");
    assert_eq!(parser.grid().row(1)[0].ch, ' ', "its newline was not the terminal's");
    assert_eq!(parser.title(), None);
}

/// A host cancel of an incomplete title discards it with the semantic collector's own framing: a
/// UTF-8 `“` (which holds 0x9c) and a newline are payload, and only its BEL ends it, with or
/// without an open kitty upload being reclaimed at the same time.
#[test]
fn cancelling_a_semantic_osc_keeps_its_framing() {
    for with_upload in [false, true] {
        let pool = CaptureStagingPool::new();
        let mut parser = Parser::new_with_staging_pool(Grid::new(16, 2), None, pool.clone());
        if with_upload {
            parser.advance(b"\x1b_Ga=T,f=100,i=7,q=2,m=1;AAAA\x1b\\");
        }
        parser.advance(b"\x1b]2;pending");
        assert!(parser.cancel_capture() > 0, "upload {with_upload}");
        parser.advance("\u{201c}tail\nmore\x07Z".as_bytes());
        assert_eq!(parser.grid().row(0)[0].ch, 'Z', "upload {with_upload}: nothing printed");
        assert_eq!(parser.title(), None, "upload {with_upload}: the cut title never applies");
    }
}

/// A cancel that lands between the ESC and the `\` of a title's ST still ends at that `\`.
#[test]
fn cancelling_a_semantic_osc_at_a_pending_esc_ends_at_its_backslash() {
    let mut parser = Parser::new(Grid::new(16, 2));
    parser.advance(b"\x1b]2;pending\x1b");
    assert!(parser.cancel_capture() > 0);
    parser.advance(b"\\Z");
    assert_eq!(parser.grid().row(0)[0].ch, 'Z');
    assert_eq!(parser.title(), None);
}

/// An ESC that does not start ST ends an accepted and a refused title alike: the ESC starts the
/// next sequence, so a BEL after it is that sequence's control and rings, and the `CSI 1 m` that
/// follows applies.
#[test]
fn an_esc_then_bel_ends_accepted_and_refused_titles_alike() {
    for payload_bytes in [8, MAX_SEMANTIC_OSC_BYTES + 1] {
        let mut parser = Parser::new(Grid::new(16, 2));
        let mut message = b"\x1b]2;".to_vec();
        message.resize(message.len() + payload_bytes, b't');
        message.extend_from_slice(b"\x1b\x07\x1b[1mX");
        let events = parser.advance(&message);
        assert!(
            events.iter().any(|event| matches!(event, VtEvent::Bell)),
            "{payload_bytes}: the BEL after the ESC rings"
        );
        let cell = &parser.grid().row(0)[0];
        assert_eq!(cell.ch, 'X', "{payload_bytes} payload bytes");
        assert!(cell.flags.contains(CellFlags::BOLD), "{payload_bytes}: the CSI applied");
        assert_eq!(parser.title(), None, "{payload_bytes}: the cut title never applies");
    }
}

/// The parser's OSC 1337 user variables as owned pairs, oldest first.
fn user_vars(parser: &Parser) -> Vec<(String, String)> {
    parser.user_vars().iter().map(|(name, value)| (name.to_owned(), value.to_owned())).collect()
}

/// `(name, value)` as the one owned pair [`user_vars`] returns.
fn one_var(name: &str, value: &str) -> Vec<(String, String)> {
    vec![(name.to_owned(), value.to_owned())]
}

/// A `SetUserVar` payload (everything after `1337;`) of exactly 64 KiB applies; one byte more is
/// refused whole, leaves the earlier variable as it was, and the text after it prints.
#[test]
fn a_user_var_payload_uses_its_exact_byte_cap() {
    let cap = crate::vt::vt_user_vars::MAX_USER_VAR_OSC_BYTES;
    for (payload_bytes, applies) in [(cap, true), (cap + 1, false)] {
        let mut parser = Parser::new(Grid::new(16, 2));
        parser.advance(b"\x1b]1337;SetUserVar=kept=MQ==\x07");
        // Padding after the record is a record that is not `SetUserVar=`, so it is skipped.
        let mut payload = b"SetUserVar=kept=Mg==;".to_vec();
        payload.resize(payload_bytes, b'x');
        let mut sequence = b"\x1b]1337;".to_vec();
        sequence.extend_from_slice(&payload);
        sequence.extend_from_slice(b"\x07Z");
        parser.advance(&sequence);
        let expected = if applies { "2" } else { "1" };
        assert_eq!(user_vars(&parser), one_var("kept", expected), "{payload_bytes} bytes");
        assert_eq!(parser.grid().row(0)[0].ch, 'Z', "{payload_bytes} bytes");
    }
}

/// CAN or SUB inside a `SetUserVar` payload, or an ESC that does not complete ST, ends it with
/// nothing applied, as for every other collected OSC; the bytes after it are ordinary output.
#[test]
fn a_cut_user_var_payload_changes_nothing() {
    for cut in [b"\x18".as_slice(), b"\x1a", b"\x1b[1m"] {
        let mut parser = Parser::new(Grid::new(16, 2));
        parser.advance(b"\x1b]1337;SetUserVar=kept=MQ==\x07");
        let mut sequence = b"\x1b]1337;SetUserVar=kept=Mg==;SetUserVar=new=Mw==".to_vec();
        sequence.extend_from_slice(cut);
        sequence.push(b'Z');
        parser.advance(&sequence);
        assert_eq!(user_vars(&parser), one_var("kept", "1"), "{cut:?}");
        assert_eq!(parser.grid().row(0)[0].ch, 'Z', "{cut:?}");
    }
}

/// The probe reads past `1337;File=`'s length only for `SetUserVar=`: a `File=` image still
/// dispatches as media and sets no variable, and a `SetUserVar` split at every byte applies once.
#[test]
fn user_vars_and_file_media_keep_separate_owners_at_every_split() {
    let pool = CaptureStagingPool::new();
    let mut parser = Parser::new_with_staging_pool(Grid::new(80, 24), None, pool.clone());
    let media = only_media(parser.advance(&iterm2_sequence(8)));
    assert_eq!(media.metadata, "File=inline=1");
    assert!(user_vars(&parser).is_empty());
    let sequence = b"\x1b]1337;SetUserVar=WEZTERM_PROG=dmlt\x1b\\Z";
    for split in 0..=sequence.len() {
        let mut parser = Parser::new_with_staging_pool(Grid::new(16, 2), None, pool.clone());
        parser.advance(&sequence[..split]);
        parser.advance(&sequence[split..]);
        assert_eq!(user_vars(&parser), one_var("WEZTERM_PROG", "vim"), "split {split}");
        assert_eq!(parser.grid().row(0)[0].ch, 'Z', "split {split}");
    }
}

/// Only the collector applies `SetUserVar`: a spelling it does not own (`01337`) reaches vte, which
/// never joins its split parameters into variables, while the canonical spelling after it applies.
#[test]
fn only_the_collector_sets_user_vars() {
    let mut parser = Parser::new(Grid::new(16, 2));
    parser.advance(b"\x1b]01337;SetUserVar=padded=MQ==\x07");
    assert!(user_vars(&parser).is_empty(), "vte's fallback set a variable");
    parser.advance(b"\x1b]1337;SetUserVar=canonical=MQ==\x07");
    assert_eq!(user_vars(&parser), one_var("canonical", "1"));
}

/// The parser's OSC 7501 records as `(id, state)`, oldest change first.
fn status_ids(parser: &Parser) -> Vec<(String, &'static str)> {
    parser
        .program_status()
        .records()
        .map(|record| (record.id().to_owned(), record.state().as_str()))
        .collect()
}

/// An OSC 7501 body of exactly 4087 wire bytes applies, under BEL and under ST; one byte more is
/// refused whole, leaves the earlier record and prints no suffix. Padding is a skipped pair, so
/// the bytes count on the wire before any filtering.
#[test]
fn a_program_status_body_uses_its_exact_wire_cap() {
    let cap = crate::vt::vt_program_status::MAX_STATUS_BODY_BYTES;
    assert_eq!(cap, 4087);
    for terminator in [b"\x07".as_slice(), b"\x1b\\"] {
        for (body_bytes, applies) in [(cap, true), (cap + 1, false)] {
            let mut parser = Parser::new(Grid::new(16, 2));
            parser.advance(b"\x1b]7501;id=kept:state=working\x07");
            let mut body = b"id=kept:state=done:pad=".to_vec();
            body.resize(body_bytes, b'\x01');
            let mut sequence = b"\x1b]7501;".to_vec();
            sequence.extend_from_slice(&body);
            sequence.extend_from_slice(terminator);
            sequence.push(b'Z');
            parser.advance(&sequence);
            let expected = if applies { "done" } else { "working" };
            let label = format!("{body_bytes} bytes, {terminator:?}");
            assert_eq!(status_ids(&parser), vec![("kept".to_owned(), expected)], "{label}");
            assert_eq!(parser.grid().row(0)[0].ch, 'Z', "{label}: no suffix printed");
        }
    }
}

/// A report split at every byte, ST included, applies once; CAN, SUB or an ESC that does not
/// complete ST ends it with nothing applied, and the bytes after it are ordinary output.
#[test]
fn a_program_status_report_splits_and_cuts_cleanly() {
    let sequence = b"\x1b]7501;id=x:state=blocked:kind=auth\x1b\\Z";
    for split in 0..=sequence.len() {
        let mut parser = Parser::new(Grid::new(16, 2));
        parser.advance(&sequence[..split]);
        parser.advance(&sequence[split..]);
        assert_eq!(status_ids(&parser), vec![("x".to_owned(), "blocked")], "split {split}");
        assert_eq!(parser.grid().row(0)[0].ch, 'Z', "split {split}");
    }
    for cut in [b"\x18".as_slice(), b"\x1a", b"\x1b[1m"] {
        let mut parser = Parser::new(Grid::new(16, 2));
        let mut sequence = b"\x1b]7501;id=x:state=working".to_vec();
        sequence.extend_from_slice(cut);
        sequence.push(b'Z');
        parser.advance(&sequence);
        assert!(status_ids(&parser).is_empty(), "{cut:?}");
        assert_eq!(parser.grid().row(0)[0].ch, 'Z', "{cut:?}");
    }
}

/// Only the collector applies OSC 7501: a spelling it does not own (`07501`) reaches vte, which
/// never stores a record, while the canonical spelling after it applies.
#[test]
fn only_the_collector_stores_program_status() {
    let mut parser = Parser::new(Grid::new(16, 2));
    parser.advance(b"\x1b]07501;state=working\x07");
    assert!(status_ids(&parser).is_empty(), "vte's fallback stored a record");
    parser.advance(b"\x1b]7501;state=working\x07");
    assert_eq!(status_ids(&parser), vec![(String::new(), "working")]);
}
