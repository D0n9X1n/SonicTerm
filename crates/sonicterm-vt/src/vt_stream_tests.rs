use super::*;
use crate::vt::vt_test_support::*;

#[test]
fn oversized_osc_is_discarded_without_unbounded_buffering() {
    let mut parser = Parser::new(Grid::new(80, 24));
    let mut payload = b"\x1b]8;;https://example.com/".to_vec();
    payload.extend(std::iter::repeat_n(b'a', MAX_ESCAPE_SEQUENCE_BYTES + 1));

    parser.advance(&payload);
    assert!(parser.discarding_oversized_escape);

    parser.advance(b"\x1b\\Z");
    assert!(!parser.discarding_oversized_escape);
    assert_eq!(parser.grid().row(0)[0].ch, 'Z');
}

#[test]
fn oversized_csi_resynchronizes_on_final_byte() {
    let mut parser = Parser::new(Grid::new(80, 24));
    let mut payload = b"\x1b[".to_vec();
    payload.extend(std::iter::repeat_n(b'1', MAX_ESCAPE_SEQUENCE_BYTES + 1));

    parser.advance(&payload);
    assert!(parser.discarding_oversized_escape);

    parser.advance(b"mZ");
    assert!(!parser.discarding_oversized_escape);
    assert_eq!(parser.grid().row(0)[0].ch, 'Z');
}

#[test]
fn can_and_sub_reset_escape_family_before_oversized_osc() {
    for cancel in [0x18, 0x1a] {
        let mut parser = Parser::new(Grid::new(80, 24));
        parser.advance(&[0x1b, b'[', b'1', cancel]);

        let mut payload = b"\x1b]0;".to_vec();
        payload.extend(std::iter::repeat_n(b'1', MAX_ESCAPE_SEQUENCE_BYTES + 1));
        payload.push(0x07);
        payload.push(b'Z');
        parser.advance(&payload);

        assert!(!parser.discarding_oversized_escape, "cancel byte {cancel:#x}");
        assert_eq!(parser.grid().row(0)[0].ch, 'Z', "cancel byte {cancel:#x}");
    }
}

/// CAN and SUB cancel a Sixel capture without emitting media and return the
/// parser to ground, so the byte after the cancel prints.
#[test]
fn can_and_sub_cancel_sixel_without_emitting_media() {
    // `ESC P ... q` opens a real Sixel capture, so it stages in a private pool.
    let pool = CaptureStagingPool::new();

    for cancel in [0x18, 0x1a] {
        let mut parser = Parser::new_with_staging_pool(Grid::new(80, 24), None, pool.clone());
        let events = parser.advance(&[0x1b, b'P', b'q', b'a', b'b', b'c', cancel, b'Z']);

        assert!(
            events.iter().all(|event| !matches!(event, VtEvent::Media(_))),
            "cancel byte {cancel:#x} emitted media"
        );
        assert_eq!(parser.grid().row(0)[0].ch, 'Z', "cancel byte {cancel:#x}");
    }
}

#[test]
fn overflow_triggering_osc_terminator_is_not_lost() {
    let mut parser = Parser::new(Grid::new(80, 24));
    let mut payload = b"\x1b]8;;".to_vec();
    payload.extend(std::iter::repeat_n(b'a', MAX_ESCAPE_SEQUENCE_BYTES - payload.len()));
    payload.push(0x07);
    payload.push(b'Z');

    parser.advance(&payload);

    assert!(!parser.discarding_oversized_escape);
    assert_eq!(parser.grid().row(0)[0].ch, 'Z');
}

#[test]
fn exact_limit_osc_resets_accounting_at_dispatch() {
    let mut parser = Parser::new(Grid::new(80, 24));
    let mut payload = b"\x1b]8;;".to_vec();
    payload.extend(std::iter::repeat_n(b'a', MAX_ESCAPE_SEQUENCE_BYTES - payload.len() - 2));
    payload.extend_from_slice(b"\x1b\\");
    assert_eq!(payload.len(), MAX_ESCAPE_SEQUENCE_BYTES);

    parser.advance(&payload);
    parser.advance(b"Z");

    assert!(!parser.discarding_oversized_escape);
    assert_eq!(parser.grid().row(0)[0].ch, 'Z');
}

#[test]
fn consecutive_ground_controls_do_not_consume_escape_budget() {
    let mut parser = Parser::new(Grid::new(80, 24));
    parser.advance(&vec![b'\n'; MAX_ESCAPE_SEQUENCE_BYTES + 1]);
    parser.advance(b"Z");

    assert!(!parser.discarding_oversized_escape);
    assert_eq!(parser.grid().row(parser.grid().rows - 1)[0].ch, 'Z');
}

#[test]
fn st_split_across_escape_limit_is_recognized() {
    let mut parser = Parser::new(Grid::new(80, 24));
    let mut payload = b"\x1b]8;;".to_vec();
    payload.extend(std::iter::repeat_n(b'a', MAX_ESCAPE_SEQUENCE_BYTES - payload.len() - 1));
    payload.push(0x1b);
    assert_eq!(payload.len(), MAX_ESCAPE_SEQUENCE_BYTES);

    parser.advance(&payload);
    parser.advance(b"\\Z");

    assert!(!parser.discarding_oversized_escape);
    assert_eq!(parser.grid().row(0)[0].ch, 'Z');
}

/// A Sixel payload longer than the generic escape-sequence limit is staged
/// against the media budget and still surfaces as a Sixel media event.
#[test]
fn large_sixel_uses_media_budget_not_generic_escape_limit() {
    let pool = CaptureStagingPool::new();
    let mut parser = Parser::new_with_staging_pool(Grid::new(80, 24), None, pool.clone());
    let mut payload = b"\x1bPq".to_vec();
    payload.extend(std::iter::repeat_n(b'?', MAX_ESCAPE_SEQUENCE_BYTES + 1));
    payload.extend_from_slice(b"\x1b\\");

    let events = parser.advance(&payload);

    let media = events
        .into_iter()
        .find_map(|event| match event {
            VtEvent::Media(media) => Some(media),
            _ => None,
        })
        .expect("large Sixel DCS should remain a media event");
    assert_eq!(media.protocol, MediaProtocol::Sixel);
    assert!(media.data.len() > MAX_ESCAPE_SEQUENCE_BYTES);
}

/// A completed CSI or OSC is a sequence boundary even when its callback leaves
/// the performer's fast-path mirror false; the following APC must still be owned.
#[test]
fn kitty_apc_dispatches_after_completed_escape_sequences() {
    let pool = CaptureStagingPool::new();
    for prefix in [b"\x1b[0m".as_slice(), b"\x1b]0;title\x07".as_slice(), b"\x1b[2;3HX".as_slice()]
    {
        let mut parser = Parser::new_with_staging_pool(Grid::new(80, 24), None, pool.clone());
        let mut sequence = prefix.to_vec();
        sequence.extend_from_slice(b"\x1b_Gf=100;AAAA\x1b\\");

        let media = only_media(parser.advance(&sequence));

        assert_eq!(media.protocol, MediaProtocol::Kitty);
        assert_eq!(
            KittyCommand::parse(media.metadata.as_bytes()).map(|command| command.format),
            Ok(100)
        );
        assert_eq!(media.data, b"AAAA");
    }

    let mut parser = Parser::new_with_staging_pool(Grid::new(80, 24), None, pool.clone());
    let events = parser.advance(b"\x1b[0m\x1b]0;title\x07\x1b[2;3H\x1b_Gf=100;AAAA\x1b\\");
    assert_eq!(only_media(events).protocol, MediaProtocol::Kitty);
}

/// C1 APC remains recognizable immediately after an ordinary ground control;
/// the control callback must not leave the fast-path mirror stale.
#[test]
fn c1_kitty_apc_dispatches_after_ground_control() {
    let pool = CaptureStagingPool::new();
    let mut parser = Parser::new_with_staging_pool(Grid::new(80, 24), None, pool.clone());

    let media = only_media(parser.advance(b"\n\x9fGf=100;AAAA\x9c"));

    assert_eq!(media.protocol, MediaProtocol::Kitty);
    assert_eq!(media.data, b"AAAA");
}

/// Both APC introducer forms and every PTY chunk boundary must reach the same
/// Kitty capture, including a split between the two bytes of ESC underscore.
#[test]
fn kitty_apc_supports_c1_and_every_buffer_split() {
    let pool = CaptureStagingPool::new();
    for sequence in [b"\x1b_Gf=100;AAAA\x1b\\".as_slice(), b"\x9fGf=100;AAAA\x9c".as_slice()] {
        for split in 0..=sequence.len() {
            let mut parser = Parser::new_with_staging_pool(Grid::new(80, 24), None, pool.clone());
            let mut events = parser.advance(&sequence[..split]);
            events.extend(parser.advance(&sequence[split..]));

            let media = only_media(events);
            assert_eq!(media.protocol, MediaProtocol::Kitty, "split {split}");
            assert_eq!(
                KittyCommand::parse(media.metadata.as_bytes()).map(|command| command.format),
                Ok(100),
                "split {split}"
            );
            assert_eq!(media.data, b"AAAA", "split {split}");
        }
    }
}

/// CAN and SUB abort Kitty staging without leaking an event or exposing the
/// remaining graphics bytes as printable terminal text.
#[test]
fn can_and_sub_cancel_kitty_without_emitting_media() {
    let pool = CaptureStagingPool::new();
    for cancel in [0x18, 0x1a] {
        let mut parser = Parser::new_with_staging_pool(Grid::new(16, 2), None, pool.clone());
        let mut sequence = vec![0x1b, b'_', b'G', b'A', b'A', cancel];
        sequence.extend_from_slice(b"Z");
        let events = parser.advance(&sequence);

        assert!(events.iter().all(|event| !matches!(event, VtEvent::Media(_))));
        assert_eq!(parser.live_capture_count(), 0);
        assert_eq!(parser.grid().row(0)[0].ch, 'Z');
    }
}

/// The parser-owned family mirror follows every vte DCS substate so C1 ST is
/// terminal only after the DCS final byte selects passthrough.
#[test]
fn escape_family_tracks_dcs_substates_and_esc_intermediates() {
    assert_eq!(EscapeFamily::Esc.after_byte(0x20), EscapeFamily::EscIntermediate);
    assert_eq!(EscapeFamily::EscIntermediate.after_byte(b'P'), EscapeFamily::Ground);

    assert_eq!(EscapeFamily::Esc.after_byte(b'P'), EscapeFamily::DcsEntry);
    assert_eq!(EscapeFamily::DcsEntry.after_byte(b'1'), EscapeFamily::DcsParam);
    assert_eq!(EscapeFamily::DcsParam.after_byte(b' '), EscapeFamily::DcsIntermediate);
    assert_eq!(EscapeFamily::DcsIntermediate.after_byte(b'0'), EscapeFamily::DcsIgnore);
    assert_eq!(EscapeFamily::DcsIgnore.after_byte(0x9c), EscapeFamily::DcsIgnore);
    assert_eq!(EscapeFamily::DcsEntry.after_byte(b'q'), EscapeFamily::DcsPassthrough);
    assert_eq!(EscapeFamily::DcsPassthrough.after_byte(0x9c), EscapeFamily::Ground);
}

/// C1 ST terminates only DCS passthrough in vte; before the final byte it must
/// not move the mirror to Ground and let controls bypass vte's DCS state.
#[test]
fn c1_st_before_dcs_hook_keeps_parser_and_vte_in_sync() {
    let mut parser = Parser::new(Grid::new(8, 3));

    parser.advance(b"\x1bP\x9cA\nB");

    assert_eq!(row_text(&parser, 0), "        ");
    assert_eq!(row_text(&parser, 1), "        ");
    parser.advance(b"\x1b\\Z");
    assert_eq!(parser.grid().row(0)[0].ch, 'Z');
}

/// Unsupported APC strings remain bounded by media staging and return cleanly
/// to ground without being misclassified as Kitty graphics.
#[test]
fn non_kitty_apc_resynchronizes_without_media() {
    let pool = CaptureStagingPool::new();
    let mut parser = Parser::new_with_staging_pool(Grid::new(16, 2), None, pool.clone());

    let events = parser.advance(b"\x1b_Xnot-kitty\x1b\\Z");

    assert!(events.iter().all(|event| !matches!(event, VtEvent::Media(_))));
    assert_eq!(parser.grid().row(0)[0].ch, 'Z');
    assert_eq!(parser.live_capture_count(), 0);
}

/// A non-Kitty APC larger than the generic escape cap still uses bounded media
/// staging, emits no image, and returns to ground after its terminator.
#[test]
fn oversized_non_kitty_apc_is_bounded_and_resynchronizes() {
    let pool = CaptureStagingPool::new();
    let mut parser = Parser::new_with_staging_pool(Grid::new(16, 2), None, pool.clone());
    let mut sequence = b"\x1b_X".to_vec();
    sequence.extend(std::iter::repeat_n(b'A', MAX_MEDIA_PAYLOAD_BYTES + 1));
    sequence.extend_from_slice(b"\x1b\\Z");

    let events = parser.advance(&sequence);

    assert!(events.iter().all(|event| !matches!(event, VtEvent::Media(_))));
    assert_eq!(parser.grid().row(0)[0].ch, 'Z');
    assert_eq!(parser.live_capture_count(), 0);
    assert_eq!(parser.retained_amount().items, 0);
}

/// A non-media C1 control executes in vte ground and must not leave the parser
/// treating the following printable text as an escape continuation.
#[test]
fn ordinary_c1_control_returns_to_printable_ground() {
    let mut parser = Parser::new(Grid::new(8, 1));

    parser.advance(b"\x80ABC");

    assert_eq!(row_text(&parser, 0), "ABC     ");
}

/// C1-looking UTF-8 continuation bytes remain owned by vte while a multibyte
/// code point is split across PTY chunks; they must not start APC or OSC media.
#[test]
fn split_utf8_continuations_are_not_media_introducers() {
    let pool = CaptureStagingPool::new();
    for text in ["\u{075d}", "\u{075f}", "\u{201d}", "\u{201f}", "\u{1f61d}", "\u{1f61f}"] {
        let bytes = text.as_bytes();
        assert!(matches!(bytes[bytes.len() - 1], 0x9d | 0x9f));
        let mut parser = Parser::new_with_staging_pool(Grid::new(8, 1), None, pool.clone());
        let mut events = Vec::new();

        for byte in bytes {
            events.extend(parser.advance(std::slice::from_ref(byte)));
        }

        assert!(events.iter().all(|event| !matches!(event, VtEvent::Media(_))));
        assert_eq!(parser.grid().row(0)[0].ch, text.chars().next().unwrap());
        assert_eq!(parser.live_capture_count(), 0);
    }
}
